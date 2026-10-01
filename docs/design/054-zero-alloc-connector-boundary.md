# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed — revised 2026-09-30 against the tree after 055
landed (router grammar, keys) and after reading both MQTT backends' data
paths.

**Scope:** the per-message path between `aimdb-core` and connectors, in both
directions: `Source`/`pump_source`, `SerializedReader`, `Connector::publish`,
`TopicProvider`, and the MQTT connector as the first adopter. SPI break in
`aimdb-core` (next major). User-facing APIs (`link_from`, `link_to`,
`with_deserializer`, `with_match_deserializer`, `with_serializer`,
`Reader::recv`) do not change.

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
| Native (`rumqttc`) | topic `publish.topic.clone()` + payload `Arc::from(publish.payload)` | `destination.to_string()` + `payload.to_vec()`, required by `AsyncClient::publish`'s owned API |
| Embedded (`mountain-mqtt` codec, own session loop) | topic `topic_name.to_string()` + payload `Payload::from` | `destination.to_string()` + `payload.to_vec()` into `AimdbMqttAction::Publish`; then `session_loop::encode` allocates a `Vec<u8>` per packet for the `Channel<Vec<u8>, 4>` write queue |

The native topic clone is avoidable today (the `Publish` is owned; move the
`String`). The embedded write queue is a third per-message allocation that
the interface change alone does not remove (§4.6).

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

**Non-goals**

- Topic grammars, wildcards and keys; 055 owns them and this design only
  carries them through the new inbound entry point.
- Making `T` delivery cheaper for heap-containing types (§4.5 is guidance).
- Allocations inside third-party client libraries.
- Remote-access JSON paths (tracked by `b0_alloc_remote_json`).

## 3. Approach

The same move as 037: object safety and `async fn` conflict, object safety
and `poll` do not. Per-message boxed futures exist only to satisfy trait
signatures, so the signatures change to poll form. Owned values that exist
only to cross the interface become borrows.

## 4. Design

### 4.1 Inbound: connectors push borrowed messages

Connectors hold each received message by reference: inside `rumqttc`'s
`Event::Incoming(Publish)`, inside the embedded session loop's decoded
`ApplicationMessage`. `Router::route(&str, &[u8], ctx)` already takes
borrows and, after 055, matches through the connector's grammar without
allocating. The interface in between is what forces owned copies (A1, A2).

Core exposes the router directly to connectors:

```rust
/// Built once per connector from the inbound links of its scheme.
pub struct InboundDispatch { /* Router + RuntimeContext */ }

impl InboundDispatch {
    /// Same validation as `AimDb::inbound_router` (055 §5.3): every link the
    /// grammar or its key cannot compile is reported.
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
}
```

- Connectors call `dispatch` wherever they hold the borrow. No future, no
  copy, no channel. `TopicMatch<'a>` already borrows the topic, so
  match-aware deserializers are unchanged (055 §7).
- `InboundDispatch` replaces `inbound_router` + `pump_source` for migrated
  connectors.
- `Source` stays for transports that naturally produce owned frames, but in
  poll form (037 pattern), which removes A1:
  `fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<(String, Payload)>>`.
  A2 remains for those transports by construction.
- Running ingest on the connector's own task is already today's behaviour
  (`pump_source` runs it inline on the reader future); it stays synchronous
  and non-blocking.

**Behaviour change (embedded).** Today the session loop parks on
`events.send(event).await` when `pump_source` falls behind, so the
`EventChannel` (`CHANNEL_SIZE` messages) absorbs bursts and throttles the
socket. With `dispatch`, a burst goes straight to the record buffers, which
drop on overflow as they already do for a slow consumer. Records that need
burst tolerance size their buffer for it.

### 4.2 Outbound: poll-based reader

`SerializedReader::recv_into` becomes a poll method over the buffer reader's
existing `poll_recv` (037). Topic and payload are both written into storage
the pump owns:

```rust
pub struct OutboundScratch {
    pub payload: Vec<u8>,   // allocated once per route (exists today)
    pub topic: String,      // new; capacity fixed once per route, never grown
}

pub enum Dest { Default, Written, Owned(String) }   // Owned: TopicProvider adapter

pub struct OutboundFrame { pub dest: Dest, pub payload: SerializedPayload }

pub trait SerializedReader: Send {
    fn poll_recv_into(
        &mut self,
        cx: &mut Context<'_>,
        ctx: &RuntimeContext,
        scratch: &mut OutboundScratch,
    ) -> Poll<DbResult<OutboundFrame>>;
}
```

- Removes A3. `SerializedPayload::Owned` stays as the fallback for
  serializers without an into-slice path (A6 remains by choice of the user).
- `Dest::Written` means `scratch.topic` holds the destination. The scratch
  is a `String` so the pump borrows it as `&str` without re-validating.
- The async `recv` / `recv_into` pair is removed rather than adapted: a poll
  method cannot wrap an async one without storing a boxed future, and the
  only implementors are in this repository (the fused reader in
  `typed_api.rs` and the bench).

### 4.3 Outbound: topics are written, not returned

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// TopicBuf implements core::fmt::Write over the route's topic scratch and
// refuses any write past its fixed capacity.
// usage: write!(out, "sensors/{}/{}", v.site, v.id)?; Ok(true)
```

- `.with_topic_writer(capacity, writer)` on the outbound builder; capacity is
  the topic scratch size for that route.
- An overflow skips the message and increments a per-link counter; the topic
  is never truncated.
- `TopicProvider` keeps working through an adapter that yields
  `Dest::Owned` (A5 remains for code that has not migrated).

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
  no locks.
- `start_send` borrows `dest` and `payload` from the pump's scratch; the
  publisher decides how to keep them (a pre-allocated ring, a client queue).
- Configuration (`qos`, `retain`, …) is parsed once in `route_publisher`,
  not per message as the MQTT sinks do today.
- **Error attribution.** A publisher that completes a hand-over
  asynchronously reports its failure from the next `poll_ready` or
  `poll_flush`, i.e. one message late. The publisher logs the failed
  destination itself (it still owns the copy); the pump only logs that the
  route saw an error.
- **Shutdown.** When the reader ends (buffer closed), the pump awaits
  `poll_flush` once before dropping the publisher, so the last message is
  not silently abandoned.
- A provided `BoxedPublisher` adapter wraps an old-style async `publish`
  for third-party connectors during migration, keeping today's cost.

With 4.2–4.4 the pump's per-message loop is:

```rust
loop {
    // Readiness first (Sink order): a blocked transport shows up as buffer
    // lag, not as one stale message held in the pump.
    poll_fn(|cx| publisher.poll_ready(cx)).await?;
    let frame = match poll_fn(|cx| reader.poll_recv_into(cx, &ctx, &mut scratch)).await {
        Ok(frame) => frame,
        Err(DbError::BufferLagged { .. }) => continue,
        Err(_) => break,
    };
    publisher.start_send(dest_of(&frame, &scratch, &default_topic), payload_of(&frame, &scratch))?;
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

**Embedded backend** (`mountain-mqtt` codec, AimDB's own `session_loop`):

- *Inbound.* `client_loop` calls `dispatch(message.topic_name,
  message.payload)` where it decodes the `ApplicationMessage` today. The
  `EventChannel`, `AimdbMqttEvent` and `MqttSource` are removed.
- *Outbound.* Removing the action copies is not enough: `perform` then
  encodes each packet into a fresh `Vec<u8>` for the write queue. Both are
  replaced by one byte ring allocated at build:
  - `start_send` encodes the complete PUBLISH packet (fixed header,
    topic, packet id, payload) directly into the ring, using
    `MqttLenWriter` for the size and `MqttBufWriter` over the reserved
    slot. The packet id is reserved from the client state at this point
    (§8).
  - `write_out` writes contiguous ring slices to the socket and releases
    them; the ring replaces `Channel<Vec<u8>, 4>` for all packets
    (publish, subscribe, ping).
  - `poll_ready` = ring has space for the route's worst-case frame
    (topic capacity + payload scratch + header).
  - The session loop no longer sees publishes as actions; it only keeps
    client state (QoS acks, keep-alive).
- Result: zero allocations per message on both directions, which also
  gives std users a fully allocation-free MQTT path through
  `TokioTcpDialer` (052).

**Native backend** (`rumqttc`):

- *Inbound.* `dispatch(&publish.topic, &publish.payload)` in the
  event-loop task; `MqttEventLoopSource` is removed.
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

`b0_alloc_connector` gains rows for the new interfaces. Targets after this
design:

| Row | Before | After |
|---|---|---|
| `inbound_route` | 0 | 0 |
| `inbound_route_pattern` | 0 | 0 |
| `inbound_route_keyed_known` | 0 | 0 |
| `inbound_route_keyed_new` | 1 | 1 (by design, 055 §5.6) |
| `inbound_dispatch` (new) | — | 0 |
| `inbound_dispatch_pattern` (new) | — | 0 |
| `inbound_pump_source_minimal` (poll `Source`) | 2 | 1 (the owned topic) |
| `outbound_scratch_static_topic` | 2 | 0 |
| `outbound_scratch_written_topic` (new) | — | 0 |
| `outbound_scratch_dynamic_topic` (`TopicProvider` adapter) | 3 | 1 |
| `outbound_owned_static_topic` | 3 | 1 |

The bench asserts exact values, so every step updates `EXPECTED` and the
baseline together. A connector-level row per MQTT backend (loopback broker
for native, the existing loopback harness for embedded) records G2; the
embedded rows target 0 in both directions, the native outbound row records
the two copies plus whatever `rumqttc` adds.

**Gate.** A CI job runs `cargo bench -p aimdb-bench --bench
b0_alloc_connector` (host only, a few seconds) and fails on any assertion.
It is added in step (1) of §6, runs on every PR from then on, and becomes a
required check in step (4). It is the counting-allocator bench, so its
results are deterministic and do not need a quiet runner.

## 6. Migration

| Who | Change |
|---|---|
| Application code | None |
| Connector authors | `inbound_router` + `pump_source` → `InboundDispatch` (or a poll `Source`); `Connector::publish` → `route_publisher` + `RoutePublisher` (or the `BoxedPublisher` adapter) |
| Custom `SerializedReader` implementors | `recv` / `recv_into` → `poll_recv_into` (none known outside this repository) |
| `TopicProvider` users | None; optional move to `TopicWriter` |

In-tree connectors to migrate and measure: MQTT (both backends), KNX,
WebSocket (client and server), the AimX session client (used by TCP, UDS and
serial), and the Embassy adapter's connector glue. The Zenoh connector
(053) is not implemented yet; it is written against `InboundDispatch` and
`RoutePublisher` from the start.

**Order:** (1) core interfaces with adapters for the old forms, bench rows
added, CI job running the bench; (2) MQTT embedded (including the write-ring
rework of §4.6), then native; (3) remaining connectors, one PR each;
(4) remove the adapters' use in-tree and make the gate a required check.

The native topic move (`publish.topic` instead of `.clone()`, §1) needs no
interface change and can land before step (1).

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
5. **Keep the embedded action channel, with fixed-size `heapless` topic and
   payload.** Removes the action allocations but not the encode `Vec`, and
   sizes every slot for the largest route. Encoding straight into the write
   ring does both.

## 8. Open questions

- **Session-task latency (embedded).** Ingest moves into the MQTT session
  task. It is synchronous and bounded, but a slow user deserializer now
  delays keep-alive handling. Measure on the STM32H5 rig (037's B3).
- **Ring sizing (embedded).** One shared ring for all routes and control
  packets, or reserved space for control packets so a stalled publisher
  cannot delay a PINGREQ; default size; how a frame larger than the ring is
  rejected (at `route_publisher` time, from the route's capacities, is
  preferred).
- **Packet ids (embedded).** Encoding in `start_send` means the publisher
  reserves a QoS 1/2 packet id outside the session loop. Either the client
  state is shared behind the ring's lock, or packet ids are patched into the
  frame by the session loop before it releases the slot to `write_out`.
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
- `aimdb-mqtt-connector/src/embedded/session_loop.rs` (`perform`, `encode`,
  `write_out`)
