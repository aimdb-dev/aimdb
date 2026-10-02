# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed — revised 2026-10-02: no backward compatibility
(adapters, `Source` and `TopicProvider` removed; one migration), embedded
MQTT outbound reworked so the session loop stays the only encoder (§4.6).
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

### 4.2 Outbound: poll-based reader

`SerializedReader` has one method, in poll form, over the buffer reader's
existing `poll_recv` (037). Topic and payload are both written into storage
the pump owns:

```rust
pub struct OutboundScratch {
    pub payload: Vec<u8>,   // allocated once per route (exists today)
    pub topic: String,      // new; capacity fixed once per route, never grown
}

pub struct OutboundFrame {
    /// `true`: `scratch.topic` holds the destination (a `TopicWriter` wrote
    /// it). `false`: use the link's default topic.
    pub written_topic: bool,
    pub payload: SerializedPayload,
}

pub trait SerializedReader: Send {
    fn poll_recv_into(
        &mut self,
        cx: &mut Context<'_>,
        ctx: &RuntimeContext,
        scratch: &mut OutboundScratch,
    ) -> Poll<DbResult<OutboundFrame>>;
}
```

- Removes A3. `SerializedPayload::Owned` stays for owned serializers (A6,
  §2 non-goals).
- The scratch topic is a `String` so the pump borrows it as `&str` without
  re-validating.
- The async `recv` / `recv_into` pair and `RecvSerializedIntoFuture` are
  removed. The only implementors are in this repository (the fused reader
  in `typed_api.rs` and the bench).

### 4.3 Outbound: topics are written, not returned

`TopicProvider` is replaced by:

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// TopicBuf implements core::fmt::Write over the route's topic scratch and
// refuses any write past its fixed capacity.
// usage: write!(out, "sensors/{}/{}", v.site, v.id)?; Ok(true)
```

- `.with_topic_writer(capacity, writer)` on the outbound builder replaces
  `.with_topic_provider(provider)`; capacity is the topic scratch size for
  that route. This is the one user-facing break in this design.
- An overflow skips the message and increments a per-link counter; the topic
  is never truncated.
- Closures implement `TopicWriter` through a blanket impl, so a one-line
  provider stays a one-line writer.
- Removes A5.

### 4.4 Outbound: per-route publishers in poll form

`Connector::publish` is shared by all routes and returns a boxed future
(A4). It is replaced by a publisher created once per route:

```rust
pub trait Connector: Send + Sync {
    /// Called once per outbound route when the pump starts.
    fn route_publisher(&self, config: &ConnectorConfig) -> Box<dyn RoutePublisher>;
}

pub trait RoutePublisher: Send {
    /// Ready to accept one message (e.g. queue space). Also drives any
    /// hand-over still in progress from the previous `start_send`.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PublishError>>;
    /// Hand one message over. Synchronous; the publisher copies what it
    /// needs into storage it owns.
    fn start_send(&mut self, dest: &str, payload: &[u8]) -> Result<(), PublishError>;
    /// Complete every hand-over started so far. Default: `Ready(Ok(()))`.
    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PublishError>> {
        let _ = cx;
        Poll::Ready(Ok(()))
    }
}
```

- This is the `futures::Sink` shape: backpressure through `poll_ready`, a
  synchronous hand-over, no future per message. `&mut self` per route means
  no locks inside the trait; a publisher that feeds shared connector state
  (§4.6) owns that synchronisation.
- `start_send` borrows `dest` and `payload` from the pump's scratch; the
  publisher decides how to keep them (a pre-allocated ring, a client queue).
- Configuration (`qos`, `retain`, …) is parsed once in `route_publisher`,
  not per message as the MQTT sinks do today. An invalid configuration is
  reported there, once, instead of failing every message.
- **Errors never end a route.** A publisher that completes a hand-over
  asynchronously reports its failure from the next `poll_ready` or
  `poll_flush`, i.e. one message late, and clears it once reported. The
  publisher logs the failed destination itself (it still owns the copy);
  the pump logs that the route saw an error and continues, as `pump_sink`
  does today.
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
    let frame = match poll_fn(|cx| reader.poll_recv_into(cx, &ctx, &mut scratch)).await {
        Ok(frame) => frame,
        Err(DbError::BufferLagged { .. }) => continue,
        Err(_) => break,
    };
    let dest = if frame.written_topic { scratch.topic.as_str() } else { &default_topic };
    if let Err(e) = publisher.start_send(dest, payload_of(&frame, &scratch)) {
        log_error!("route '{}': publish to '{}' failed: {:?}", default_topic, dest, e);
    }
}
let _ = poll_fn(|cx| publisher.poll_flush(cx)).await;
```

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

So route publishers do not encode packets. The hand-over is split across two
pre-allocated rings, and the session loop stays the only encoder:

```text
RoutePublisher × N ──copy──▶ record ring ──▶ session loop ──encode──▶ write ring ──▶ write_out ──▶ socket
  (qos, retain, topic,        (per connector,   (sole owner of           (per connection,
   payload)                    survives          ClientStateNoQueue,      bip buffer)
                               reconnects)       same gate as today)
```

- **Record ring** (replaces `ActionChannel`). One per connector, a byte ring
  of variable-length records `{qos, retain, topic_len, payload_len, topic,
  payload}`, allocated at build. `start_send` copies one record in;
  `poll_ready` = room for this route's largest record (header + topic
  capacity + payload scratch capacity, known at `route_publisher` time).
  Publishers waiting for room register in a multi-waker; the session loop
  waits on a signal raised by `start_send`. The ring lives as long as the
  connector, so records queued during an outage are sent after reconnecting,
  as actions are today.
- **Session loop.** Where `perform` runs today, the loop peeks the next
  record when `action_ready` holds and the write ring has room for its
  encoded size (`MqttLenWriter`), calls `state.publish_packet(topic,
  payload, qos, retain)`, encodes the packet straight into the write ring
  (`MqttBufWriter` over the reserved slot), calls `state.publish_update`,
  then releases the record. Packet ids, the one-in-flight rule and the
  connect/subscribe ordering are untouched.
- **Write ring** (replaces `Channel<Vec<u8>, 4>` and `encode`'s `Vec`). One
  per connection, cleared on reconnect so no bytes from an old session
  reach a new socket. All packets go through it (CONNECT, SUBSCRIBE,
  PUBLISH, PUBACK, PINGREQ). `MqttBufWriter` needs a contiguous slice, so
  the ring is a bip buffer: a frame that does not fit before the end skips
  to the start. `write_out` writes contiguous slices to the socket and
  releases them.
- **Control packets.** The session loop encodes a PUBLISH only when the
  write ring keeps a reserve for control packets (PUBACK, PINGREQ), so a
  stalled socket full of publishes cannot block an acknowledgement. PINGREQ
  stays lossy, as today.
- **Oversize frames.** `route_publisher` rejects a route whose largest
  record cannot fit in the record ring, or whose encoded frame cannot fit in
  the write ring minus the control reserve.

Cost: two copies of topic and payload per message (into the record, then
into the frame), zero allocations. This is the price of keeping the state
machine single-owner; it also gives std users an allocation-free MQTT path
through `TokioTcpDialer` (052).

#### Native backend (`rumqttc`)

- *Inbound.* `dispatch(&publish.topic, &publish.payload)` in the event-loop
  task; `MqttEventLoopSource` is removed. This removes both the topic clone
  and the `Arc` payload copy.
- *Outbound.* `AsyncClient` takes owned `String` and payload, so the two
  copies stay (G2). Two options for the publisher, to be settled when the
  native backend migrates:
  1. `start_send` builds the `AsyncClient::publish` future and stores it in
     a per-route `tokio_util::sync::ReusableBoxFuture` (037's Tokio
     technique); `poll_ready` / `poll_flush` drive it. Keeps backpressure,
     removes the box, reports errors one message late (§4.4).
  2. `start_send` calls the synchronous `AsyncClient::try_publish`. No
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
embedded rows target 0 in both directions, the native outbound row records
the two copies plus whatever `rumqttc` adds.

**Gate.** A CI job runs `cargo bench -p aimdb-bench --bench
b0_alloc_connector` (host only, a few seconds) and fails on any assertion.
It lands first, against today's interfaces (§6 step 1), and is a required
check from then on. It is the counting-allocator bench, so its results are
deterministic and do not need a quiet runner.

## 6. Migration

| Who | Change |
|---|---|
| Application code | `.with_topic_provider(p)` → `.with_topic_writer(capacity, w)`. Nothing else. |
| Connector authors | `inbound_router` + `pump_source` / `router.route` → `InboundDispatch`; `Connector::publish` → `route_publisher` + `RoutePublisher`. No adapters. |
| Custom `SerializedReader` implementors | `recv` / `recv_into` → `poll_recv_into` (none known outside this repository) |

Without adapters, changing the core traits breaks every connector at once,
so the work lands as one change (a feature branch merged once):

| Crate | Inbound | Outbound |
|---|---|---|
| `aimdb-core` | `InboundDispatch`; remove `Source`, `pump_source`, public `inbound_router`; AimX session client (TCP, UDS, serial) moves from `router.route` | poll `SerializedReader`, `TopicWriter`, `RoutePublisher`, new pump; remove `Connector::publish`, `TopicProvider` |
| `aimdb-mqtt-connector` embedded | `dispatch` in `drain_packets` | record ring + write ring (§4.6) |
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
   no gain over `poll_ready`/`start_send`.
5. **Embedded: keep the action channel, with fixed-size `heapless` topic and
   payload.** Removes the action allocations but not the encode `Vec`, and
   sizes every slot for the largest route. The record ring (§4.6) holds
   variable-length records instead.
6. **Embedded: route publishers encode PUBLISH into the write ring
   (previous revision of this design).** One copy instead of two, but
   publishers would need the session's client state: whether it is
   connected and subscribed, whether the single in-flight slot is free, and
   the next packet id. Frames encoded during an outage would reach a new
   socket ahead of CONNECT. Sharing the state across tasks needs a lock, and
   resetting the ring on reconnect loses queued messages. Keeping the session
   loop as the only encoder avoids all of it for one extra memcpy.
7. **Keep compatibility adapters (`BoxedPublisher`, poll `Source`,
   `TopicProvider` adapter) for a staged migration.** Allows one PR per
   connector, but keeps two mechanisms per direction and their bench rows.
   Every connector is in this repository, so one migration is cheaper.

## 8. Open questions

- **Ingest latency on the transport task.** Embedded: a slow user
  deserializer delays keep-alive and ack handling in the session loop.
  Native: it delays polling `rumqttc`'s event loop. Measure on the STM32H5
  rig (037's B3) and with the native loopback row; decide whether to
  document a deserializer budget.
- **Ring sizing (embedded).** Defaults for the record ring and the write
  ring, and the size of the write ring's control reserve.
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
