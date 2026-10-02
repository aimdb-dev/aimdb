# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed — revised 2026-10-02: no backward compatibility
(adapters, `Source` and `TopicProvider` removed; one migration); publishers
lend the buffers the reader serializes into (§4.2, §4.4); embedded MQTT
outbound uses one lock-free record ring per route and keeps the session loop
as the only encoder (§4.6).
Previous revision 2026-09-30, after 055 landed.

**Scope:** the per-message path between `aimdb-core` and connectors, in both
directions: `Source`/`pump_source`, `SerializedReader`, `Connector::publish`,
`TopicProvider`, and every in-tree connector. Breaking change to the
connector SPI and to one user-facing builder method: `with_topic_provider`
is replaced by `with_topic_writer` (§4.3). `link_from`, `link_to`,
`with_deserializer`, `with_match_deserializer`, `with_serializer`,
`with_serializer_into` and `Reader::recv` do not change.

**Compatibility:** none kept. There are no adapters for the old traits; all
in-tree connectors migrate in one change (§6).

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
interns its name (055 §5.6), and a known key costs nothing. Every other
non-zero row is caused by the connector interface:

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

Both native inbound copies disappear with §4.1. The embedded write queue is
a third per-message allocation that the interface change alone does not
remove (§4.6).

Not every connector goes through `Source` today: the AimX session client
(TCP, UDS, serial) and both WebSocket builders already call
`Router::route` with borrows from their own frames. MQTT (both backends),
KNX and the Embassy adapter's glue use `Source` + `pump_source`.

**Values with heap data.** Each reader receives its own clone of `T`
(`T: Clone` delivery). A value with one `String` field costs one allocation
per reader per message (observed with 1 and 3 readers; not a committed bench
row). This is a property of the buffer contract, not of the connector
boundary; §4.5 covers it as guidance.

**CI today.** The bench asserts its `EXPECTED` values when run, but CI only
builds it (`make` runs `cargo build --package aimdb-bench --benches`). Nothing
gates the numbers yet.

## 2. Goals and non-goals

**Goals**

- G1. Zero AimDB-added allocations per message in steady state on both
  connector directions, for scratch serializers and static or written
  topics. "Steady state" excludes the first sighting of a key (055 §5.6).
- G2. The MQTT connector adds no copies or allocations beyond what its
  client library's API requires, documented per backend.
- G3. `b0_alloc_connector` runs in CI and gates the result.
- G4. One mechanism per direction: a single inbound entry point and a single
  outbound publisher trait, with no compatibility layers.

**Non-goals**

- Topic grammars, wildcards and keys; 055 owns them and this design only
  carries them through the new inbound entry point.
- Making `T` delivery cheaper for heap-containing types (§4.5 is guidance).
- Removing owned serializers (A6). `with_serializer` stays: it is used far
  more than `with_serializer_into` in-tree (63 call sites against 7), and
  requiring a declared capacity everywhere belongs in its own design. Users
  who need zero allocations choose `with_serializer_into`.
- Allocations inside third-party client libraries.
- Remote-access JSON paths (tracked by `b0_alloc_remote_json`).

## 3. Approach

The same move as 037: object safety and `async fn` conflict, object safety
and `poll` do not. Per-message boxed futures exist only to satisfy trait
signatures, so the signatures change to poll form. Owned values that exist
only to cross the interface become borrows.

Because no compatibility is kept, each direction gets exactly one shape:
inbound connectors push borrowed messages into `InboundDispatch`; outbound
connectors implement `RoutePublisher`. `Source`, `pump_source`,
`Connector::publish`, `TopicProvider` and the async `SerializedReader`
methods are removed, not adapted.

## 4. Design

### 4.1 Inbound: connectors push borrowed messages

Connectors hold each received message by reference: inside `rumqttc`'s
`Event::Incoming(Publish)`, inside the embedded session loop's decoded
`ApplicationMessage`, inside a decoded AimX or WebSocket frame.
`Router::route(&str, &[u8], ctx)` already takes borrows and, after 055,
matches through the connector's grammar without allocating. The `Source`
interface in between is what forces owned copies (A1, A2).

Core exposes the router to connectors through one type:

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

- Connectors call `dispatch` wherever they hold the borrow. No future, no
  copy, no channel. `TopicMatch<'a>` already borrows the topic, so
  match-aware deserializers are unchanged (055 §7).
- `InboundDispatch` is the only inbound entry point. `Source`,
  `pump_source` and `AimDb::inbound_router` are removed; `Router` becomes
  crate-private. A transport that produces owned frames calls `dispatch`
  with borrows of its own frame, so A2 no longer exists as a category.
- For the AimX session client and the WebSocket connectors this is a rename:
  they already call `router.route(topic, payload, &ctx)`.
- `InboundDispatch` is cheap to share (`Arc` inside or `Clone`), since the
  WebSocket server dispatches from one task per client.

**Behaviour changes.**

- *Embedded MQTT: burst absorption.* Today the session loop parks on
  `events.send(event).await` when `pump_source` falls behind, so the
  `EventChannel` (`CHANNEL_SIZE` messages) absorbs bursts and throttles the
  socket. With `dispatch`, a burst goes straight to the record buffers,
  which drop on overflow as they already do for a slow consumer. Records
  that need burst tolerance size their buffer for it.
- *QoS 1 inbound is acknowledge-then-maybe-drop.* `drain_packets` queues
  the PUBACK before the message is delivered, and the broker does not
  resend an acknowledged message. Today an acknowledged message has at least
  reached the event channel; with `dispatch` it can be dropped at once by a
  full record buffer. QoS 1 at the transport therefore means "reached
  AimDB", not "reached every record". This is documented on the MQTT
  connector; moving the PUBACK after `dispatch` does not change it, because
  `dispatch` does not report per-record drops.
- *Ingest runs on the transport task.* For embedded MQTT that is the session
  loop; for native MQTT it is the task polling `rumqttc`'s event loop. A
  slow user deserializer delays keep-alive and ack handling in both (§8).

### 4.2 Outbound: the publisher lends the buffers

The serialized topic and payload are written straight into storage the
publisher owns. For the embedded MQTT backend that storage is a slot in the
route's record ring (§4.6), so the bytes are never copied between
serialization and encoding. For other connectors it is a per-route scratch
the publisher allocates once, which replaces the pump's scratch of today.

```rust
/// Where the reader writes one message. Borrowed from the publisher.
pub struct OutboundBuffers<'a> {
    pub topic: TopicBuf<'a>,    // capacity = the route's topic capacity (§4.3)
    pub payload: &'a mut [u8],  // capacity = the route's payload capacity
}

/// What the reader wrote.
pub struct OutboundFrame {
    /// `Some(len)`: a `TopicWriter` wrote `len` bytes into `buffers.topic`.
    /// `None`: use the route's default topic.
    pub topic: Option<usize>,
    pub payload: CommittedPayload,
}

pub enum CommittedPayload {
    /// `len` bytes written into `buffers.payload` (`with_serializer_into`).
    InPlace(usize),
    /// An owned serializer's bytes (`with_serializer`, A6). The publisher
    /// moves or copies them as its transport needs.
    Owned(Vec<u8>),
}

pub trait SerializedReader: Send {
    fn poll_recv_into(
        &mut self,
        cx: &mut Context<'_>,
        ctx: &RuntimeContext,
        buffers: OutboundBuffers<'_>,
    ) -> Poll<DbResult<OutboundFrame>>;
}
```

- Removes A3. The reader runs the topic writer and the serializer only when
  a value is ready, so it writes into `buffers` at most once per `Ready`.
- `CommittedPayload::Owned` keeps owned serializers working (A6, §2
  non-goals). It is also the cheaper path for transports whose client takes
  an owned payload: the native MQTT publisher moves the `Vec` into
  `rumqttc` instead of copying it (§4.6).
- The async `recv` / `recv_into` pair, `RecvSerializedIntoFuture` and
  `SerializedPayload` are removed. The only implementors are in this
  repository (the fused reader in `typed_api.rs` and the bench).

### 4.3 Outbound: topics are written, not returned

`TopicProvider` is replaced by:

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// TopicBuf implements core::fmt::Write over a byte slice the publisher lends
// (§4.2) and refuses any write past its end. Only `&str` goes in, so the
// bytes are valid UTF-8 by construction.
// usage: write!(out, "sensors/{}/{}", v.site, v.id)?; Ok(true)
```

- `.with_topic_writer(capacity, writer)` on the outbound builder replaces
  `.with_topic_provider(provider)`; capacity is the topic space the publisher
  reserves per message on that route. This is the one user-facing break in
  this design.
- An overflow skips the message and increments a per-link counter; the topic
  is never truncated.
- Closures implement `TopicWriter` through a blanket impl, so a one-line
  provider stays a one-line writer.
- Removes A5.

### 4.4 Outbound: per-route publishers in poll form

`Connector::publish` is shared by all routes and returns a boxed future
(A4). It is replaced by a publisher created once per route:

```rust
/// Everything a publisher needs to know about its route, fixed at build.
pub struct RouteSpec<'a> {
    pub default_topic: &'a str,
    pub config: &'a ConnectorConfig,  // qos, retain, … from the link URL
    pub topic_capacity: usize,        // 0 without a TopicWriter
    /// `Some(n)`: into-slice serializer with an n-byte scratch.
    /// `None`: owned serializer; the publisher picks its own limit.
    pub payload_capacity: Option<usize>,
}

pub trait Connector: Send + Sync {
    /// Called once per outbound route while the connector builds its pumps,
    /// before its transport tasks start.
    fn route_publisher(&self, route: &RouteSpec<'_>)
        -> Result<Box<dyn RoutePublisher>, PublishError>;
}

pub trait RoutePublisher: Send {
    /// Ready to accept one message: storage for it is reserved. Also drives
    /// any hand-over still in progress from the previous `commit`.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PublishError>>;
    /// The storage reserved by the last successful `poll_ready`. Calling it
    /// again before `commit` returns the same storage.
    fn buffers(&mut self) -> OutboundBuffers<'_>;
    /// The message written into `buffers` is complete. Synchronous.
    fn commit(&mut self, frame: OutboundFrame) -> Result<(), PublishError>;
    /// Complete every hand-over committed so far. Default: `Ready(Ok(()))`.
    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PublishError>> {
        let _ = cx;
        Poll::Ready(Ok(()))
    }
}
```

- This is the `futures::Sink` shape with the buffer lent out instead of
  passed in (the grant pattern of `bbqueue`): backpressure through
  `poll_ready`, a synchronous hand-over, no future per message, no copy into
  the publisher.
- A reservation is held while the reader waits for the next value. That is
  why it belongs to one route: a publisher must not reserve in storage other
  routes share (§4.6 gives each route its own ring).
- `route_publisher` parses configuration (`qos`, `retain`, …) once, not per
  message as the MQTT sinks do today, and rejects a route it cannot serve
  (invalid configuration, a message that can never fit) before anything
  runs. It returns a `Result` for that reason.
- **Errors never end a route.** A publisher that completes a hand-over
  asynchronously reports its failure from the next `poll_ready` or
  `poll_flush`, i.e. one message late, and clears it once reported. The
  publisher logs the failed destination itself; the pump logs that the route
  saw an error and continues, as `pump_sink` does today. A message that does
  not fit (topic overflow, an owned payload larger than the publisher's
  limit) is skipped and counted, and the reservation is reused.
- **Shutdown.** When the reader ends (buffer closed), the pump awaits
  `poll_flush` once before dropping the publisher, so the last message is
  not silently abandoned.
- There is no adapter for the old `publish`. Every in-tree connector
  implements `RoutePublisher` directly (§6).

With 4.2–4.4 the pump's per-message loop is:

```rust
loop {
    // Readiness first (Sink order): a blocked transport shows up as buffer
    // lag, not as one stale message held in the pump. An error here belongs
    // to an earlier message.
    if let Err(e) = poll_fn(|cx| publisher.poll_ready(cx)).await {
        log_warn!("route '{}': earlier publish failed: {:?}", default_topic, e);
        continue;
    }
    // Serialize straight into the publisher's reserved storage.
    let frame = match poll_fn(|cx| reader.poll_recv_into(cx, &ctx, publisher.buffers())).await {
        Ok(frame) => frame,
        Err(DbError::BufferLagged { .. }) => continue, // reservation kept
        Err(_) => break,
    };
    if let Err(e) = publisher.commit(frame) {
        log_error!("route '{}': publish failed: {:?}", default_topic, e);
    }
}
let _ = poll_fn(|cx| publisher.poll_flush(cx)).await;
```

The pump no longer owns any per-route buffer; `RouteSpec` carries the sizes
the old pump scratch was built from.

### 4.5 Values with heap data (guidance)

Delivery clones `T` once per reader. For records with more than one reader
at message rate:

- prefer `Copy` values (numbers, fixed-size arrays, `heapless::String<N>`);
- or wrap the value in `Arc<T>`, so each reader's clone is a reference-count
  increment;
- keep rarely changing, heap-heavy data in a separate record.

Document this next to the buffer types; no API change.

### 4.6 MQTT connector

#### Embedded backend (`mountain-mqtt` codec, AimDB's own `session_loop`)

*Inbound.* `drain_packets` calls `dispatch(publish.topic_name(),
publish.payload())` where it decodes the publish today. `EventChannel`,
`AimdbMqttEvent`, `Received::Event` and `MqttSource` are removed.

*Outbound.* The session loop's client state (`ClientStateNoQueue`) decides
when a PUBLISH may go out, and it is the only thing that may assign packet
ids:

```rust
// session_loop.rs today: the action arm is armed only when
let action_ready = connected
    && !state.waiting_for_responses()   // one in-flight slot
    && next_topic >= subscribe_topics.len()
    && !outbound.is_full();
```

So route publishers do not encode packets. Each route serializes into its
own record ring; the session loop stays the only encoder and writes into
one write ring:

```text
route 1 pump ──serialize in place──▶ record ring 1 ─┐
route 2 pump ──serialize in place──▶ record ring 2 ─┼─▶ session loop ──encode (1 copy)──▶ write ring ──▶ write_out ──▶ socket
route N pump ──serialize in place──▶ record ring N ─┘   (sole owner of                    (per connection,
               (one producer each, lock-free,            ClientStateNoQueue,               bip buffer)
                survives reconnects)                     same gate as today)
```

- **Record rings** (replace `ActionChannel`). One per route, created in
  `route_publisher` while the connector builds its pumps, so the session
  loop starts with a fixed list of ring consumers. Each ring is a
  single-producer, single-consumer bip buffer of variable-length records
  `{topic_len, payload_len, topic, payload}`; `qos`, `retain` and the
  default topic are stored once per ring, not per record.
  - `poll_ready` reserves one contiguous slot of `topic_capacity +
    payload_capacity` (plus a small record header) and waits if the ring has
    no room.
  - `buffers` lends the slot's topic and payload regions; the serializer
    and `TopicWriter` write directly into them.
  - `commit` writes the record header with the real lengths and releases the
    unused tail of the slot. `CommittedPayload::Owned` is copied into the
    slot (A6 keeps its copy); an owned payload larger than the slot is
    skipped and counted.
  - Records queued during an outage stay in their rings and are sent after
    reconnecting, as actions are today.
- **Synchronisation.** No lock. Each record ring has exactly one producer
  (its route's pump) and one consumer (the session loop), so head and tail
  are atomics. The pump parks on the ring's single waker when it is full;
  `commit` raises one signal the session loop waits on. Neither
  `ClientStateNoQueue` nor the write ring is shared: the session loop owns
  the state and is the write ring's only writer, `write_out` its only
  reader.
- **Session loop.** Where `perform` runs today, when `action_ready` holds
  the loop takes the oldest committed record from the rings in round-robin
  order, checks the write ring has room for its encoded size
  (`MqttLenWriter`), calls `state.publish_packet(topic, payload, qos,
  retain)` with slices borrowed from the record ring, encodes the packet
  straight into the write ring (`MqttBufWriter` over the reserved slot),
  calls `state.publish_update`, then releases the record. Round-robin keeps
  one busy route from starving the others; packet ids, the one-in-flight
  rule and the connect/subscribe ordering are untouched.
- **Write ring** (replaces `Channel<Vec<u8>, 4>` and `encode`'s `Vec`). One
  per connection, cleared on reconnect so no bytes from an old session
  reach a new socket. All packets go through it (CONNECT, SUBSCRIBE,
  PUBLISH, PUBACK, PINGREQ). `MqttBufWriter` needs a contiguous slice, so
  it is a bip buffer as well. `write_out` writes contiguous slices to the
  socket and releases them.
- **Control packets.** The session loop encodes a PUBLISH only when the
  write ring keeps a reserve for control packets (PUBACK, PINGREQ), so a
  stalled socket full of publishes cannot block an acknowledgement. PINGREQ
  stays lossy, as today.
- **Sizes and oversize routes.** A route's record ring holds at least two
  slots, so the pump can fill the next record while the loop encodes the
  previous one. For an owned serializer (`payload_capacity: None`), the slot
  uses the connector's maximum payload, a builder option. `route_publisher`
  rejects a route whose encoded frame cannot fit in the write ring minus the
  control reserve.

Cost per message: zero allocations and one copy of topic and payload (record
ring into the encoded frame), in addition to the network stack's own copy
into its TX buffer, which TLS needs anyway to encrypt. The remaining copy is
the price of keeping packet encoding inside the session loop (§7,
alternative 8). This also gives std users an allocation-free MQTT path
through `TokioTcpDialer` (052).

#### Native backend (`rumqttc`)

- *Inbound.* `dispatch(&publish.topic, &publish.payload)` in the event-loop
  task; `MqttEventLoopSource` is removed. This removes both the topic clone
  and the `Arc` payload copy.
- *Outbound.* `AsyncClient` takes an owned `String` and payload (G2). The
  publisher lends a per-route topic and payload scratch from `buffers`;
  `commit` builds the owned topic and copies the in-place payload, or moves a
  `CommittedPayload::Owned` `Vec` straight in without copying. Two options
  for the hand-over, to be settled when the native backend migrates:
  1. `commit` builds the `AsyncClient::publish` future and stores it in
     a per-route `tokio_util::sync::ReusableBoxFuture` (037's Tokio
     technique); `poll_ready` / `poll_flush` drive it. Keeps backpressure,
     removes the box, reports errors one message late (§4.4).
  2. `commit` calls the synchronous `AsyncClient::try_publish`. No
     stored future, errors attributed immediately, but no readiness signal:
     a full request channel drops the message and counts it.

  Option 1 matches today's behaviour (`publish` waits for channel space)
  and is the default.
- `rumqttc`'s own allocations (building `Publish`, its request channel) are
  outside G1.

## 5. Measurement and gate

`b0_alloc_connector` is rewritten against the new interfaces. Targets after
this design:

| Row | Before | After |
|---|---|---|
| `inbound_dispatch` (was `inbound_route`) | 0 | 0 |
| `inbound_dispatch_pattern` (was `inbound_route_pattern`) | 0 | 0 |
| `inbound_dispatch_keyed_known` | 0 | 0 |
| `inbound_dispatch_keyed_new` | 1 | 1 (by design, 055 §5.6) |
| `inbound_pump_source_minimal` | 2 | removed (`Source` is gone) |
| `outbound_scratch_static_topic` | 2 | 0 |
| `outbound_scratch_written_topic` (replaces `outbound_scratch_dynamic_topic`) | 3 | 0 |
| `outbound_owned_static_topic` | 3 | 1 (A6, by choice of serializer) |

The bench asserts exact values, so every change updates `EXPECTED` and the
baseline together. A connector-level row per MQTT backend (loopback broker
for native, the existing loopback harness for embedded) records G2; the
embedded rows target 0 allocations in both directions and record the one
outbound copy (§4.6); the native outbound row records the owned topic, the
payload copy and whatever `rumqttc` adds.

**Gate.** A CI job runs `cargo bench -p aimdb-bench --bench
b0_alloc_connector` (host only, a few seconds) and fails on any assertion.
It lands first, against today's interfaces (§6 step 1), and is a required
check from then on. It is the counting-allocator bench, so its results are
deterministic and do not need a quiet runner.

## 6. Migration

| Who | Change |
|---|---|
| Application code | `.with_topic_provider(p)` → `.with_topic_writer(capacity, w)`. Nothing else. |
| Connector authors | `inbound_router` + `pump_source` / `router.route` → `InboundDispatch`; `Connector::publish` → `route_publisher(&RouteSpec)` + `RoutePublisher` (`poll_ready`, `buffers`, `commit`, `poll_flush`); the publisher owns the per-route buffers the pump used to own. No adapters. |
| Custom `SerializedReader` implementors | `recv` / `recv_into` → `poll_recv_into` (none known outside this repository) |

Without adapters, changing the core traits breaks every connector at once,
so the work lands as one change (a feature branch merged once):

| Crate | Inbound | Outbound |
|---|---|---|
| `aimdb-core` | `InboundDispatch`; remove `Source`, `pump_source`, public `inbound_router`; AimX session client (TCP, UDS, serial) moves from `router.route` | poll `SerializedReader`, `TopicWriter`, `RoutePublisher`, new pump; remove `Connector::publish`, `TopicProvider` |
| `aimdb-mqtt-connector` embedded | `dispatch` in `drain_packets` | per-route record rings + write ring (§4.6) |
| `aimdb-mqtt-connector` native | `dispatch` in event-loop task | `ReusableBoxFuture` publisher |
| `aimdb-knx-connector` | `Source` → `dispatch` | `RoutePublisher` |
| `aimdb-websocket-connector` | rename (`router.route` → `dispatch`), client and server | server `RoutePublisher` |
| `aimdb-embassy-adapter` | `send_wrapper.rs` wrappers for `Source` / `Connector` removed or reduced to `RoutePublisher` | — |
| tests, bench | codec tests that build a router to ingest (`aimdb-data-contracts/src/link_codec.rs`, `aimdb-mqtt-connector/tests/link_ext_tests.rs`) → `InboundDispatch` (`route_count`, `subscriptions`, `dispatch`) | `topic_provider_tests.rs` (mqtt, knx), websocket e2e → `TopicWriter`; bench rewritten (§5) |

**Order:**

1. On main, independently: the CI gate for today's bench, and the native
   MQTT topic move (`publish.topic` instead of `.clone()`).
2. On the feature branch: core interfaces and pump, bench rewritten.
3. MQTT embedded (§4.6), then native.
4. KNX, WebSocket, Embassy adapter, data contracts, tests.
5. Merge once the gate and every connector's tests pass.

The Zenoh connector (053) is not implemented yet; it is written against
`InboundDispatch` and `RoutePublisher` from the start.

## 7. Alternatives considered

1. **`async fn` in traits with generic pumps.** Removes boxes by
   monomorphization, but the pumps are type-erased per route (`dyn
   SerializedSource`), and `Send` bounds on the returned futures need extra
   machinery. Poll form keeps `dyn` and is already the codebase's pattern
   (037).
2. **`ReusableBoxFuture` everywhere.** One allocation per route instead of
   per message, but it needs `unsafe` or `tokio-util` on `no_std` (037 §3.2
   rejected the hand-rolled version) and keeps an indirection per poll.
3. **Lending async `Source` (`async fn next(&mut self) -> Option<Inbound<'_>>`).**
   The borrow must outlive `.await`, which conflicts with how both MQTT
   clients expose messages. Push (`dispatch`) fits them directly.
4. **Keep `Connector::publish` and pool its futures.** Futures of different
   connectors have different sizes; pooling adds unsafe layout handling for
   no gain over `poll_ready`/`commit`.
5. **Embedded: keep the action channel, with fixed-size `heapless` topic and
   payload.** Removes the action allocations but not the encode `Vec`, and
   sizes every slot for the largest route. The record ring (§4.6) holds
   variable-length records instead.
6. **Embedded: route publishers encode PUBLISH into the write ring
   (first revision of this design).** Publishers would need the session's
   client state: whether it is connected and subscribed, whether the single
   in-flight slot is free, and the next packet id. Frames encoded during an
   outage would reach a new socket ahead of CONNECT. Sharing the state
   across tasks needs a lock, and resetting the ring on reconnect loses
   queued messages. It has the same single copy as §4.6, so it gains nothing
   for its problems.
7. **Keep compatibility adapters (`BoxedPublisher`, poll `Source`,
   `TopicProvider` adapter) for a staged migration.** Allows one PR per
   connector, but keeps two mechanisms per direction and their bench rows.
   Every connector is in this repository, so one migration is cheaper.
8. **Embedded: zero AimDB copies by hand-encoding PUBLISH in the record
   ring.** Lay each record out as a frame with gaps (fixed header and topic
   length in front, a hole for the packet id), let the session loop patch
   only the packet id, and have `write_out` send straight from the record
   rings. Removes the last copy, but AimDB would encode PUBLISH outside
   `mountain-mqtt`'s codec and must stay byte-identical to it (MQTT 5
   properties included), `write_out` would interleave N record rings and the
   control ring through a descriptor queue, and records would stay reserved
   until the socket write completes, so rings grow. The copy it saves is one
   memcpy of topic and payload, small next to the network stack's own copy
   and TLS encryption. Revisit only if the STM32H5 rig shows the copy
   matters.
9. **Publisher takes borrowed bytes (`start_send(&str, &[u8])`, second
   revision of this design).** Simpler trait, but the pump serializes into
   its own scratch and every publisher copies from it, so the embedded
   backend pays two copies. With one shared record ring for all routes it
   also needs a lock among the route pumps. Lending the buffers (§4.4) and
   one ring per route remove both.

## 8. Open questions

- **Ingest latency on the transport task.** Embedded: a slow user
  deserializer delays keep-alive and ack handling in the session loop.
  Native: it delays polling `rumqttc`'s event loop. Measure on the STM32H5
  rig (037's B3) and with the native loopback row; decide whether to
  document a deserializer budget.
- **Ring sizing (embedded).** Slots per record ring (at least two), the
  default maximum payload for owned-serializer routes, the write ring size
  and its control reserve. Per-route rings cost more RAM than one shared
  ring; measure with the demo applications' route counts.
- **Ring implementation.** The record rings need a single-producer,
  single-consumer bip buffer with grants and runtime-chosen sizes. Check
  whether `bbqueue` fits (`no_std`, waker integration, storage sized at
  build rather than by const generic) before writing one.
- **Other connectors' own copies.** KNX, WebSocket and the AimX session were
  not measured at connector level; §5's per-connector rows will show them.

## 9. References

- [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md)
- [045 — Per-link codec selection](./045-per-link-codec-selection.md)
  (`with_serializer_into` scratch path)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, `pump_sink`, `StreamDialer`)
- [053 — Zenoh connector](./053-zenoh-connector.md)
- [055 — Wildcard inbound links](./055-wildcard-inbound-links.md)
  (grammar, keys, `TopicMatch`, §7 relation to this design)
- `aimdb-bench/benches/b0_alloc_connector.rs`
- `aimdb-mqtt-connector/src/embedded/session_loop.rs` (`client_loop`,
  `drain_packets`, `perform`, `encode`, `write_out`)
