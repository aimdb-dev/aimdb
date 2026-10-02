# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed — rewritten 2026-10-02 around a pull model: each
connector's own task drives both directions; core runs no per-route tasks.
Earlier revisions (per-route push pumps with `RoutePublisher`, per-route
record rings) are in this branch's history and summarised in §7.

**Scope:** the per-message path between `aimdb-core` and connectors, in both
directions, and every in-tree connector. Breaking change to the connector
SPI and to one user-facing builder method: `with_topic_provider` is replaced
by `with_topic_writer` (§4.3). `link_from`, `link_to`, `with_deserializer`,
`with_match_deserializer`, `with_serializer`, `with_serializer_into` and
`Reader::recv` do not change.

**Compatibility:** none kept. No adapters for the old traits; all in-tree
connectors migrate in one change (§6).

**Builds on:** [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md),
which made buffers and the consume path allocation-free and left the
connector boundary open (037 §2, "Outbound connector"; §3.3), and
[055 — Wildcard inbound links](./055-wildcard-inbound-links.md), which gave
the inbound router a connector-supplied grammar and per-record keys.

---

## 1. Measured state

Baseline: `cargo bench -p aimdb-bench --bench b0_alloc_connector`
(committed in `aimdb-bench/data/baselines/b0_alloc_connector.json`), Tokio
current-thread runtime, 2,000 messages after warm-up, no-op connector.

| Path | Allocs/msg | Bytes/msg |
|---|---|---|
| `Router::route`, exact topic (64 routes, deserialize + produce) | **0** | 0 |
| `Router::route`, pattern (`{device}`, MQTT grammar) | **0** | 0 |
| `Router::route`, keyed pattern, known key | **0** | 0 |
| `Router::route`, keyed pattern, new key every message | 1 | 187 |
| `pump_source` with a minimal `Source` | **2** | 25 |
| Outbound `recv_into` + `publish`, scratch serializer, static topic | **2** | 73 |
| same, dynamic topic (`TopicProvider`) | **3** | 81 |
| same, owned serializer | **3** | 81 |

Buffers, the consume path (037) and routing/ingest allocate nothing in
steady state. The keyed-new row is by design: the first sighting of a key
interns its name (055 §5.6). Every other non-zero row is caused by the
connector interface:

| # | Direction | Cause | Where |
|---|---|---|---|
| A1 | in | `Source::next` returns a boxed future | `session/mod.rs` (`BoxFut`) |
| A2 | in | `Source::next` returns an owned topic `String` | `session/mod.rs` |
| A3 | out | `SerializedReader::recv_into` returns a boxed future | `connector.rs` (`RecvSerializedIntoFuture`) |
| A4 | out | `Connector::publish` returns a boxed future | `transport.rs` |
| A5 | out | `TopicProvider::topic` returns `Option<String>` | `connector.rs` |
| A6 | out | owned serializer returns `Vec<u8>` (only without `with_serializer_into`) | `typed_api.rs` |

**Connector-side copies and allocations** (read from code, not yet measured
at connector level):

| Backend | Inbound | Outbound |
|---|---|---|
| Native (`rumqttc`) | topic `publish.topic.clone()` + payload `Arc::from(publish.payload.as_ref())` | `destination.to_string()` + `payload.to_vec()`, required by `AsyncClient::publish`'s owned API |
| Embedded (`mountain-mqtt` codec, own session loop) | topic `topic_name.to_string()` + payload `Payload::from` | `destination.to_string()` + `payload.to_vec()` into `AimdbMqttAction::Publish`; then `session_loop::encode` allocates a `Vec<u8>` per packet for the `Channel<Vec<u8>, 4>` write queue |

**Structure today.** Every connector already owns at least one transport
task (MQTT event loop or session loop, KNX connection task, WebSocket
server, AimX session). Core adds one `pump_source` task per connector and
one `pump_sink` task per outbound route; the outbound pumps all funnel into
the same transport (one MQTT action channel, one socket). The AimX session
client and both WebSocket builders already call `Router::route` with
borrows instead of using `Source`.

**Values with heap data.** Each reader receives its own clone of `T`
(`T: Clone` delivery). A value with one `String` field costs one allocation
per reader per message (observed with 1 and 3 readers; not a committed bench
row). This is a property of the buffer contract, not of the connector
boundary; §4.5 covers it as guidance.

**CI today.** The bench asserts its `EXPECTED` values when run, but CI only
builds it (`make` runs `cargo build --package aimdb-bench --benches`).
Nothing gates the numbers yet.

## 2. Goals and non-goals

**Goals**

- G1. Zero AimDB-added allocations per message in steady state on both
  connector directions, for scratch serializers and static or written
  topics. "Steady state" excludes the first sighting of a key (055 §5.6).
- G2. The MQTT connector adds no copies or allocations beyond what its
  client library's API requires, documented per backend.
- G3. `b0_alloc_connector` runs in CI and gates the result.
- G4. The connector SPI is two objects, one per direction, and core runs no
  tasks of its own for a connector. No compatibility layers.

**Non-goals**

- Topic grammars, wildcards and keys; 055 owns them and this design only
  carries them through the inbound entry point.
- Making `T` delivery cheaper for heap-containing types (§4.5 is guidance).
- Removing owned serializers (A6). `with_serializer` stays: it is used far
  more than `with_serializer_into` in-tree (63 call sites against 7), and
  requiring a declared capacity everywhere belongs in its own design.
- Parallel sends across routes of one connector. Every in-tree transport
  serialises sends anyway (one socket, one MQTT in-flight slot); §4.6.
- Allocations inside third-party client libraries.
- Remote-access JSON paths (tracked by `b0_alloc_remote_json`).

## 3. Approach

The connector's transport task is the only task that touches its
transport. It already receives inbound messages; under this design it also
decides when to send. Core gives it two objects built from the database and
the connector's scheme:

- `InboundDispatch`: the connector pushes each received message, as
  borrows, and core deserializes and produces it synchronously.
- `OutboundRoutes`: when the transport can take a message, the connector
  pulls the next ready one from any of its routes, already serialized into
  storage `OutboundRoutes` owns.

The record buffers (SPMC ring, mailbox, single-latest) are the outbound
queue. Nothing is queued between them and the transport, so no per-route
task, ring, channel or publisher trait is needed. Backpressure is the
connector not pulling; a record whose link falls behind lags exactly as a
slow reader does today.

Per-message boxed futures disappear because no trait signature returns a
future any more: the connector's loop is concrete code and awaits its own
client directly. Owned values that only existed to cross the interface
become borrows.

## 4. Design

### 4.1 Inbound: connectors push borrowed messages

Connectors hold each received message by reference: inside `rumqttc`'s
`Event::Incoming(Publish)`, inside the embedded session loop's decoded
publish, inside a decoded AimX or WebSocket frame. `Router::route(&str,
&[u8], ctx)` already takes borrows and, after 055, matches through the
connector's grammar without allocating.

```rust
/// Built once per connector from the inbound links of its scheme.
pub struct InboundDispatch { /* Router + RuntimeContext */ }

impl InboundDispatch {
    /// Same validation as today's `AimDb::inbound_router` (055 §5.3): every
    /// link the grammar or its key cannot compile is reported.
    pub fn new(
        db: &AimDb,
        scheme: &str,
        grammar: &'static dyn TopicGrammar,
    ) -> DbResult<Self>;
    /// Match, deserialize and produce into every matching record.
    /// Synchronous, allocation-free for known keys, never blocks (full
    /// buffers drop and log, full key tables drop and count, as today).
    pub fn dispatch(&self, topic: &str, payload: &[u8]);
    /// Filters to subscribe at the transport (`Router::subscriptions`).
    /// Called once at connect time; allocates, which is fine there.
    pub fn subscriptions(&self) -> Vec<Arc<str>>;
    /// Number of inbound routes, for logging (`Router::route_count`).
    pub fn route_count(&self) -> usize;
}
```

- `TopicMatch<'a>` already borrows the topic, so match-aware deserializers
  are unchanged (055 §7).
- `Source`, `pump_source` and `AimDb::inbound_router` are removed; `Router`
  becomes crate-private. A transport that produces owned frames calls
  `dispatch` with borrows of its own frame, so A1 and A2 disappear.
- For the AimX session client and the WebSocket connectors this is a
  rename. `InboundDispatch` is `Clone` (shared `Arc` inside), since the
  WebSocket server dispatches from one task per client.

### 4.2 Outbound: connectors pull serialized messages

```rust
/// Every outbound link of one scheme, read by the connector's transport task.
pub struct OutboundRoutes { /* routes, round-robin cursor, scratch buffers */ }

pub struct RouteInfo {
    pub id: RouteId,                 // dense index, 0..len
    pub default_topic: Arc<str>,     // from the link URL
    pub config: ConnectorConfig,     // qos, retain, record_index, …
}

pub struct Outbound<'a> {
    pub route: &'a RouteInfo,
    /// The written topic (`TopicWriter`) or the route's default.
    pub topic: &'a str,
    pub payload: OutboundPayload<'a>,
}

pub enum OutboundPayload<'a> {
    /// Serialized into `OutboundRoutes`' scratch (`with_serializer_into`).
    Borrowed(&'a [u8]),
    /// An owned serializer's bytes (`with_serializer`, A6). Transports that
    /// take ownership move it in; others borrow it.
    Owned(Vec<u8>),
}

impl OutboundRoutes {
    /// Subscribes every outbound link of `scheme`; each route's cursor starts
    /// here, so build it when the transport task starts. Allocates the
    /// scratch once, sized to the largest route's topic and payload capacity.
    pub fn new(db: &AimDb, scheme: &str) -> DbResult<Self>;
    /// Routes, for parsing per-route configuration once at start.
    pub fn routes(&self) -> &[RouteInfo];
    /// The next ready message from any route, round-robin.
    /// `Ready(None)`: every route is closed.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Outbound<'_>>>;
    /// `poll_fn(|cx| self.poll_next(cx))`.
    pub async fn next(&mut self) -> Option<Outbound<'_>>;
}
```

**Semantics of `poll_next`.**

- Starting after the route that produced the last message, it polls each
  open route's typed reader (`poll_recv`, 037). The first value that is
  ready is serialized (topic writer, then serializer) into the scratch and
  returned. Pending readers register `cx`, so any of them waking wakes the
  transport task.
- A value is taken from its buffer only in a call that returns `Ready`.
  `poll_next` is therefore safe as a `select` arm: a losing arm takes
  nothing.
- `BufferLagged` is logged and that route is polled again; a closed buffer
  closes the route; topic overflow and serializer errors skip the value,
  log, and count per link. None of these end the other routes.
- One message is borrowed at a time (`&mut self`). The connector finishes
  with it (copies, encodes, or awaits its send) before pulling the next.
- Cost of a wake-up: one `poll_recv` per open route. Fine for tens of
  routes; §8 notes a ready set for many more.

`SerializedReader`, `SerializedPayload`, `RecvSerializedIntoFuture` and
`pump_sink` are removed. The typed reader + serializer pair stays inside
core as the per-route state of `OutboundRoutes`.

### 4.3 Outbound: topics are written, not returned

`TopicProvider` is replaced by:

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// TopicBuf implements core::fmt::Write over the scratch topic region and
// refuses any write past the route's capacity. Only `&str` goes in, so the
// bytes are valid UTF-8 by construction.
// usage: write!(out, "sensors/{}/{}", v.site, v.id)?; Ok(true)
```

- `.with_topic_writer(capacity, writer)` on the outbound builder replaces
  `.with_topic_provider(provider)`. This is the one user-facing break in
  this design.
- An overflow skips the message and increments a per-link counter; the topic
  is never truncated.
- Closures implement `TopicWriter` through a blanket impl.
- Removes A5.

### 4.4 Connector shape

A connector builds both objects when its transport task starts and drives
them from that task. For a client with an async send, the outbound half is
a plain loop:

```rust
let mut outbound = OutboundRoutes::new(&db, "mqtt")?;
let opts: Vec<PublishOpts> = outbound.routes().iter().map(PublishOpts::parse).collect::<Result<_, _>>()?;

while let Some(msg) = outbound.next().await {
    let o = &opts[msg.route.id];
    if let Err(e) = client.publish(msg.topic, o.qos, o.retain, msg.payload).await {
        log_error!("publish to '{}' failed: {:?}", msg.topic, e);
    }
}
```

- The `.await` is concrete code in the connector's own task: no boxed
  future, no stored future, errors attributed to the message that caused
  them.
- Configuration is parsed once per route at start; an invalid link is
  reported before anything runs.
- A connector that must send while also reading (one socket, one task)
  puts `poll_next` in its `select`, gated by its own readiness (§4.7).
- Shutdown: when `next` returns `None` every outbound record is gone; the
  connector flushes its transport as it sees fit.

Removed from the SPI: `Connector::publish`, `Source`, `pump_source`,
`pump_sink`, `SerializedReader` and `TopicProvider`. The `Vec<BoxFuture>`
connectors return today shrinks to their own transport tasks.

### 4.5 Values with heap data (guidance)

Delivery clones `T` once per reader. For records with more than one reader
at message rate:

- prefer `Copy` values (numbers, fixed-size arrays, `heapless::String<N>`);
- or wrap the value in `Arc<T>`, so each reader's clone is a reference-count
  increment;
- keep rarely changing, heap-heavy data in a separate record.

Document this next to the buffer types; no API change.

### 4.6 Behaviour changes

- **Outbound queueing moves into the record buffers.** Today a message
  leaves its buffer as soon as its route's pump runs and waits in the
  connector (MQTT action channel, `CHANNEL_SIZE` deep) until the transport
  can send. Now it stays in the record buffer until the connector pulls.
  During an outage or a slow link, what is sent afterwards depends on the
  record's buffer: single-latest sends only the newest value, a mailbox its
  last, an SPMC ring its backlog or a lag report. Document this per buffer
  type on the link builder.
- **One send at a time per connector.** Routes of one connector share the
  transport task, so a slow send delays all of them. That is already true
  underneath for every in-tree transport; the per-route pumps only hid it.
  A future transport with genuinely parallel sends spawns its own workers
  from its loop.
- **Fairness.** Round-robin over ready routes; a busy route cannot starve
  the others. Priorities are not in scope.
- **Ingest and serialization run on the transport task.** Inbound
  deserialization already did for `pump_source`'s connectors; outbound
  serialization now does too. A slow user (de)serializer delays the
  transport's keep-alive and ack handling (§8).
- **Embedded MQTT: burst absorption.** Today the session loop parks on
  `events.send(event).await` when `pump_source` falls behind, so the
  `EventChannel` absorbs bursts and throttles the socket. With `dispatch`,
  a burst goes straight to the record buffers, which drop on overflow as
  they already do for a slow consumer.
- **QoS 1 inbound is acknowledge-then-maybe-drop.** `drain_packets` queues
  the PUBACK before the message is delivered, and the broker does not
  resend an acknowledged message. With `dispatch` an acknowledged message
  can be dropped at once by a full record buffer. QoS 1 at the transport
  means "reached AimDB", not "reached every record"; documented on the MQTT
  connector.

### 4.7 MQTT connector

#### Embedded backend (`mountain-mqtt` codec, AimDB's own `session_loop`)

*Inbound.* `drain_packets` calls `dispatch(publish.topic_name(),
publish.payload())` where it decodes the publish today. `EventChannel`,
`AimdbMqttEvent`, `Received::Event` and `MqttSource` are removed.

*Outbound.* The session loop's action arm pulls from `OutboundRoutes`
instead of the `ActionChannel`, under the same gate as today:

```rust
let action_ready = connected
    && !state.waiting_for_responses()          // one in-flight slot
    && next_topic >= subscribe_topics.len()
    && write_ring.has_room(max_frame + CONTROL_RESERVE);
let action_arm = poll_fn(|cx| {
    if action_ready { outbound.poll_next(cx) } else { Poll::Pending }
});
// on Some(msg):
//   packet = state.publish_packet(msg.topic, payload, qos[msg.route.id], retain[..])
//   encode packet into the write ring (the one copy), state.publish_update(&packet)
```

- `MqttSink`, `AimdbMqttAction::Publish` and the `ActionChannel` are
  removed. Subscriptions keep their current path (the loop already places
  them itself).
- **Write ring** (replaces `encode`'s `Vec` and `Channel<Vec<u8>, 4>`). A
  `bbqueue` 0.7 stream queue, allocated once at build. The session loop
  writes every packet (CONNECT, SUBSCRIBE, PUBLISH, PUBACK, PINGREQ) with
  `grant_exact`, sizing it with `MqttLenWriter` and encoding with
  `MqttBufWriter` over the grant; `write_out` takes `read()` grants and
  releases them after `write_all`. Both ends run in the same task under the
  existing `select3`, so the ring needs no lock.
- **Control reserve.** For a PUBLISH the loop takes `grant_exact(frame_len +
  CONTROL_RESERVE)` and commits only `frame_len`, so PUBACK and PINGREQ
  always find room. PINGREQ stays lossy, as today.
- **Reconnect.** When a connection ends, the loop drains the write ring
  through its consumer before the next CONNECT, so no bytes from an old
  session reach a new socket. Unsent outbound values are still in their
  record buffers (§4.6).
- **Oversize.** `max_frame` comes from the largest route's capacities; a
  route whose frame cannot fit in the write ring minus the reserve is
  rejected when the connector builds. An owned payload larger than the
  write ring allows is skipped and counted.
- `bbqueue = { version = "0.7", default-features = false, features =
  ["alloc"] }`; `AtomicCoord` where the target has atomic pointers,
  `portable-atomic` features on `thumbv6m`. Only the polling notifier is
  needed: `write_out` and the session loop share a task, and the loop wakes
  `write_out` with a waker it already holds.

Cost per message: zero allocations and one copy of topic and payload
(scratch into the encoded frame), besides the network stack's own copy into
its TX buffer, which TLS needs anyway to encrypt. This also gives std users
an allocation-free MQTT path through `TokioTcpDialer` (052).

#### Native backend (`rumqttc`)

- *Inbound.* `dispatch(&publish.topic, &publish.payload)` in the event-loop
  task; `MqttEventLoopSource` is removed. This removes both the topic clone
  and the `Arc` payload copy.
- *Outbound.* One task runs the loop of §4.4 over `AsyncClient::publish`.
  `rumqttc` takes an owned `String` and payload (G2): the topic is copied,
  a borrowed payload is copied, an `OutboundPayload::Owned` `Vec` is moved
  in. `publish` waits for request-channel space, which is the backpressure.
- `rumqttc`'s own allocations (building `Publish`, its request channel) are
  outside G1.

## 5. Measurement and gate

`b0_alloc_connector` is rewritten against the new interfaces. Targets:

| Row | Before | After |
|---|---|---|
| `inbound_dispatch` (was `inbound_route`) | 0 | 0 |
| `inbound_dispatch_pattern` | 0 | 0 |
| `inbound_dispatch_keyed_known` | 0 | 0 |
| `inbound_dispatch_keyed_new` | 1 | 1 (by design, 055 §5.6) |
| `inbound_pump_source_minimal` | 2 | removed (`Source` is gone) |
| `outbound_next_static_topic` (was `outbound_scratch_static_topic`) | 2 | 0 |
| `outbound_next_written_topic` (replaces the `TopicProvider` row) | 3 | 0 |
| `outbound_next_owned` (was `outbound_owned_static_topic`) | 3 | 1 (A6, by choice of serializer) |
| `outbound_next_round_robin` (new; 8 routes, all ready) | — | 0 |

The bench asserts exact values, so every change updates `EXPECTED` and the
baseline together. A connector-level row per MQTT backend (loopback broker
for native, the existing loopback harness for embedded) records G2: the
embedded rows target 0 allocations in both directions and record the one
outbound copy; the native outbound row records the owned topic, the payload
copy and whatever `rumqttc` adds.

**Gate.** A CI job runs `cargo bench -p aimdb-bench --bench
b0_alloc_connector` (host only, a few seconds) and fails on any assertion.
It lands first, against today's interfaces (§6 step 1), and is a required
check from then on. It is the counting-allocator bench, so its results are
deterministic and do not need a quiet runner.

## 6. Migration

| Who | Change |
|---|---|
| Application code | `.with_topic_provider(p)` → `.with_topic_writer(capacity, w)`. Nothing else. |
| Connector authors | Build `InboundDispatch` and `OutboundRoutes` in the transport task; call `dispatch` on receive, pull with `next`/`poll_next` when the transport can send. `Source`, `pump_source`, `pump_sink`, `Connector::publish` are gone. |

Without adapters the core change breaks every connector at once, so the
work lands as one change (a feature branch merged once):

| Crate | Inbound | Outbound |
|---|---|---|
| `aimdb-core` | `InboundDispatch`; remove `Source`, `pump_source`, public `inbound_router`; AimX session client moves from `router.route` | `OutboundRoutes`, `TopicWriter`; remove `pump_sink`, `Connector::publish`, `SerializedReader`, `TopicProvider`; AimX session client pulls in its session task |
| `aimdb-mqtt-connector` embedded | `dispatch` in `drain_packets` | action arm pulls from `OutboundRoutes`; `bbqueue` write ring |
| `aimdb-mqtt-connector` native | `dispatch` in event-loop task | publish loop task over `AsyncClient` |
| `aimdb-knx-connector` | `Source` → `dispatch` | pull in its connection task |
| `aimdb-websocket-connector` | rename (`router.route` → `dispatch`), client and server | server: broadcast loop pulls (`record_index` from `RouteInfo.config`) |
| `aimdb-embassy-adapter` | `send_wrapper.rs` wrappers for `Source` / `Connector` removed | — |
| tests, bench | codec tests that build a router (`aimdb-data-contracts/src/link_codec.rs`, `aimdb-mqtt-connector/tests/link_ext_tests.rs`) → `InboundDispatch` | `topic_provider_tests.rs` (mqtt, knx), websocket e2e → `TopicWriter`; KNX task-count tests drop the per-route publisher; bench rewritten (§5) |

**Order:**

1. On main, independently: the CI gate for today's bench, and the native
   MQTT topic move (`publish.topic` instead of `.clone()`).
2. On the feature branch: `InboundDispatch`, `OutboundRoutes`,
   `TopicWriter`; bench rewritten.
3. MQTT embedded in two steps, each tested with the loopback harness:
   1. the `bbqueue` write ring, replacing `encode`'s `Vec` and the
      `Vec` channel (touches `perform`, `drain_packets`, `queue`,
      `write_out`);
   2. the action arm pulls from `OutboundRoutes`; `ActionChannel` and
      `MqttSink` removed.
4. MQTT native, KNX, WebSocket, AimX session client, Embassy adapter,
   tests.
5. Merge once the gate and every connector's tests pass.

The Zenoh connector (053) is not implemented yet; it is written against
`InboundDispatch` and `OutboundRoutes` from the start.

## 7. Alternatives considered

1. **`async fn` in traits with generic pumps.** Removes boxes by
   monomorphization, but the pumps are type-erased per route, and `Send`
   bounds on the returned futures need extra machinery. The pull model has
   no trait that returns a future at all.
2. **`ReusableBoxFuture` everywhere.** One allocation per route instead of
   per message, but it needs `unsafe` or `tokio-util` on `no_std` (037 §3.2
   rejected the hand-rolled version) and keeps an indirection per poll.
3. **Lending async `Source` (`async fn next(&mut self) -> Option<Inbound<'_>>`).**
   The borrow must outlive `.await`, which conflicts with how both MQTT
   clients expose messages. Push (`dispatch`) fits them directly.
4. **Per-route push pumps with a poll-form publisher (earlier revisions of
   this design).** Core keeps one pump task per outbound route; each
   connector implements a `futures::Sink`-shaped `RoutePublisher`
   (`poll_ready`, `start_send` or `buffers`/`commit`, `poll_flush`), with a
   `SendBytes` helper for copying transports. Reaches the same allocation
   numbers, but every connector implements a publisher trait, native MQTT
   needs a stored `ReusableBoxFuture` and reports errors one message late,
   and the embedded backend needs a queue between the pumps and the session
   loop because the pumps run independently of it. That queue is either a
   shared ring behind a lock with one extra copy, or one lock-free record
   ring per route with lent buffers, a custom `bbqueue` notifier and
   round-robin inside the session loop. The pull model removes the pumps,
   so none of that is needed: the record buffers are the queue.
5. **Embedded: route publishers encode PUBLISH into the write ring (first
   revision).** Publishers would need the session's client state (connected
   and subscribed, the single in-flight slot, the next packet id), frames
   encoded during an outage would reach a new socket ahead of CONNECT, and
   sharing the state across tasks needs a lock. In the pull model the
   session loop encodes, so the question does not arise.
6. **Keep compatibility adapters for a staged migration.** Allows one PR per
   connector, but keeps two mechanisms per direction. Every connector is in
   this repository, so one migration is cheaper.
7. **Embedded: zero AimDB copies by hand-encoding PUBLISH.** Serialize the
   topic and payload straight into a write-ring grant at offsets left for
   the fixed header, topic length and packet id, then fill those in. Removes
   the last copy, but AimDB would encode PUBLISH outside `mountain-mqtt`'s
   codec and must stay byte-identical to it (MQTT 5 properties included).
   The copy it saves is one memcpy of topic and payload, small next to the
   network stack's copy and TLS encryption. Revisit only if the STM32H5 rig
   shows the copy matters; the pull model makes it a local change to the
   session loop.

## 8. Open questions

- **(De)serialization latency on the transport task.** Embedded: a slow
  user serializer or deserializer delays keep-alive and ack handling in the
  session loop. Native: it delays polling `rumqttc`'s event loop or the
  publish loop. Measure on the STM32H5 rig (037's B3) and with the native
  loopback row; decide whether to document a budget.
- **Many routes.** A wake-up polls every open route. If a connector has
  hundreds of outbound links, give `OutboundRoutes` a ready set fed by
  per-route wakers instead of polling all of them.
- **Outage semantics per buffer type.** §4.6 changes what is sent after an
  outage. Confirm the documented behaviour per buffer type with the
  loopback harness, including SPMC lag reporting.
- **Write ring sizing (embedded).** Default size and `CONTROL_RESERVE`.
- **Other connectors' own copies.** KNX, WebSocket and the AimX session were
  not measured at connector level; §5's per-connector rows will show them.
- **Inbound chunk channel (embedded).** The `read_into` side (`Channel<Chunk,
  1>` into `PacketReader`) could read socket bytes straight into a `bbqueue`
  grant. Out of scope; noted for a follow-up.

## 9. References

- [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md)
- [045 — Per-link codec selection](./045-per-link-codec-selection.md)
  (`with_serializer_into` scratch path)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, `pump_sink`, `StreamDialer`)
- [053 — Zenoh connector](./053-zenoh-connector.md)
- [055 — Wildcard inbound links](./055-wildcard-inbound-links.md)
  (grammar, keys, `TopicMatch`, §7 relation to this design)
- [`bbqueue`](https://github.com/jamesmunns/bbqueue) 0.7
- `aimdb-bench/benches/b0_alloc_connector.rs`
- `aimdb-mqtt-connector/src/embedded/session_loop.rs` (`client_loop`,
  `drain_packets`, `perform`, `encode`, `write_out`)
