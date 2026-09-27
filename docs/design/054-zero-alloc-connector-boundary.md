# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed

**Scope:** the per-message path between `aimdb-core` and connectors, in both
directions: `Source`/`pump_source`, `SerializedReader`, `Connector::publish`,
`TopicProvider`, and the MQTT connector as the first adopter. SPI break in
`aimdb-core` (next major). User-facing APIs (`link_from`, `link_to`,
`with_deserializer`, `with_serializer`, `Reader::recv`) do not change.

**Builds on:** [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md),
which made buffers and the consume path allocation-free and left the
connector boundary open (037 §2, "Outbound connector"; §3.3).

---

## 1. Measured state

Baseline: `cargo bench -p aimdb-bench --bench b0_alloc_connector`
(committed in `aimdb-bench/data/baselines/b0_alloc_connector.json`), Tokio
current-thread runtime, 2,000 messages after warm-up, no-op connector.

| Path | Allocs/msg | Bytes/msg |
|---|---|---|
| `Router::route` (64 routes, deserialize + produce) | **0** | 0 |
| `pump_source` with a minimal `Source` | **2** | 25 |
| Outbound `recv_into` + `publish`, scratch serializer, static topic | **2** | 73 |
| same, dynamic topic (`TopicProvider`) | **3** | 81 |
| same, owned serializer | **3** | 81 |

Buffers, the consume path (037) and routing/ingest allocate nothing. Every
non-zero row is caused by the connector interface:

| # | Direction | Cause | Where |
|---|---|---|---|
| A1 | in | `Source::next` returns a boxed future | `session/mod.rs` (`BoxFut`) |
| A2 | in | `Source::next` returns an owned topic `String` | `session/mod.rs` |
| A3 | out | `SerializedReader::recv_into` returns a boxed future | `connector.rs` (`RecvSerializedIntoFuture`) |
| A4 | out | `Connector::publish` returns a boxed future | `transport.rs` |
| A5 | out | `TopicProvider::topic` returns `Option<String>` | `connector.rs` |
| A6 | out | owned serializer returns `Vec<u8>` (only without `with_serializer_into`) | `typed_api.rs` |

**Connector-side copies** (read from code, same traits): the MQTT connector
adds one payload copy inbound (`Arc::from(payload)`, both backends) and two
copies outbound (`destination.to_string()`, `payload.to_vec()`, both
backends). With `rumqttc` the outbound copies are required by its owned-value
`publish` API; on the embedded backend they exist to pass an owned action
through a channel to the session task.

**Values with heap data.** Each reader receives its own clone of `T`
(`T: Clone` delivery). A value with one `String` field costs one allocation
per reader per message (measured with 1 and 3 readers). This is a property of
the buffer contract, not of the connector boundary; §4.5 covers it as
guidance.

## 2. Goals and non-goals

**Goals**

- G1. Zero AimDB-added allocations per message on both connector directions,
  for scratch serializers and static or written topics.
- G2. The MQTT connector adds no copies beyond what its client library's API
  requires, documented per backend.
- G3. `b0_alloc_connector` gates the result in CI.

**Non-goals**

- Topic wildcards and templates. See [055](./055-wildcard-inbound-links.md).
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
`Event::Incoming(Publish)`, inside `mountain-mqtt`'s application-message
handler. `Router::route(&str, &[u8], ctx)` already takes borrows and does not
allocate. The interface in between is what forces owned copies (A1, A2).

Core exposes the router directly to connectors:

```rust
/// Built once per connector from the inbound routes of its scheme.
pub struct InboundDispatch { /* Router + RuntimeContext */ }

impl InboundDispatch {
    pub fn new(db: &AimDb, scheme: &str) -> Self;
    /// Deserialize and produce into every matching record. Synchronous,
    /// allocation-free, never blocks (full buffers drop and log, as today).
    pub fn dispatch(&self, topic: &str, payload: &[u8]);
    /// Topics to subscribe at the transport.
    pub fn resource_ids(&self) -> &[Arc<str>];
}
```

- Connectors call `dispatch` wherever they hold the borrow. No future, no
  copy, no channel.
- `Source` stays for transports that naturally produce owned frames, but in
  poll form (037 pattern), which removes A1:
  `fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<(String, Payload)>>`.
  A2 remains for those transports by construction.
- Running ingest on the connector's own task is already today's behaviour
  (`pump_source` runs it inline on the reader future); it stays synchronous
  and non-blocking.

### 4.2 Outbound: poll-based reader

`SerializedReader::recv_into` becomes a poll method over the buffer reader's
existing `poll_recv` (037). Topic and payload are both written into storage
the pump owns:

```rust
pub struct OutboundScratch {
    pub payload: Vec<u8>,   // allocated once per route (exists today)
    pub topic: Vec<u8>,     // new; allocated once per route
}

pub enum Dest { Default, Written(usize), Owned(String) }   // Owned: TopicProvider adapter

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

Removes A3. `SerializedPayload::Owned` stays as the fallback for serializers
without an into-slice path (A6 remains by choice of the user).

### 4.3 Outbound: topics are written, not returned

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// TopicBuf implements core::fmt::Write over the route's topic scratch.
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
    /// Ready to accept one message (e.g. queue space).
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PublishError>>;
    /// Hand one message over. Synchronous; the publisher copies what it
    /// needs into storage it owns.
    fn start_send(&mut self, dest: &str, payload: &[u8]) -> Result<(), PublishError>;
}
```

- This is the `futures::Sink` shape: backpressure through `poll_ready`, a
  synchronous hand-over, no future per message. `&mut self` per route means
  no locks.
- `start_send` borrows `dest` and `payload` from the pump's scratch; the
  publisher decides how to keep them (a pre-allocated ring, a client queue).
- Configuration (`qos`, `retain`, …) is parsed once in `route_publisher`,
  not per message as the MQTT sinks do today.
- A provided `BoxedPublisher` adapter wraps an old-style async `publish`
  for third-party connectors during migration, keeping today's cost.

With 4.2–4.4 the pump's per-message loop is:

```rust
let frame = poll_fn(|cx| reader.poll_recv_into(cx, &ctx, &mut scratch)).await?;
poll_fn(|cx| publisher.poll_ready(cx)).await?;
publisher.start_send(dest_of(&frame, &scratch, &default_topic), payload_of(&frame, &scratch))?;
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

| Backend | Inbound | Outbound |
|---|---|---|
| Embedded (`mountain-mqtt`) | `dispatch` from the application-message handler inside the session task; the event channel for inbound messages is removed | `start_send` copies topic and payload into a byte ring allocated at build; the session task reads frames from it. `poll_ready` = ring space |
| Native (`rumqttc`) | `dispatch(&publish.topic, &publish.payload)` in the event-loop task | `rumqttc::AsyncClient::publish` takes owned `String` and payload, so the two copies stay. Its future is kept in a per-route `tokio_util::sync::ReusableBoxFuture` (037's Tokio technique), which removes the box but not the copies |

`rumqttc`'s own allocations (building `Publish`, its request channel) are
outside G1. The embedded backend also runs on Tokio through
`TokioTcpDialer` (052), which gives std users a fully allocation-free MQTT
path when they need it.

## 5. Measurement and gate

`b0_alloc_connector` gains rows for the new interfaces. Targets after this
design:

| Row | Before | After |
|---|---|---|
| `inbound_route` | 0 | 0 |
| `inbound_dispatch` (new) | — | 0 |
| `inbound_pump_source_minimal` (poll `Source`) | 2 | 1 (the owned topic) |
| `outbound_scratch_static_topic` | 2 | 0 |
| `outbound_scratch_written_topic` (new) | — | 0 |
| `outbound_scratch_dynamic_topic` (`TopicProvider` adapter) | 3 | 1 |
| `outbound_owned_static_topic` | 3 | 1 |

The bench asserts exact values, so every step updates `EXPECTED` and the
baseline together. A connector-level row per MQTT backend (loopback broker
for native, the existing loopback harness for embedded) records G2. The gate
becomes required on PRs touching `aimdb-core/src/{connector,transport,router}.rs`,
`session/`, or a connector's data path.

## 6. Migration

| Who | Change |
|---|---|
| Application code | None |
| Connector authors | `impl Source` → `InboundDispatch` or poll `Source`; `Connector::publish` → `route_publisher` + `RoutePublisher` (or the `BoxedPublisher` adapter) |
| `TopicProvider` users | None; optional move to `TopicWriter` |

In-tree connectors to migrate and measure: MQTT (both backends), KNX,
WebSocket (client and server), the AimX session client (used by TCP, UDS and
serial), and the Embassy adapter's connector glue.

**Order:** (1) core interfaces with adapters for the old forms, bench rows
added; (2) MQTT embedded, then native; (3) remaining connectors, one PR each;
(4) remove the adapters' use in-tree and make the gate required.

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

## 8. Open questions

- **Session-task latency (embedded).** Ingest moves into the MQTT session
  task. It is synchronous and bounded, but a slow user deserializer now
  delays keep-alive handling. Measure on the STM32H5 rig (037's B3).
- **Ring sizing (embedded outbound).** One shared ring or one per route;
  default size; what `poll_ready` reports when one frame exceeds the ring.
- **Other connectors' own copies.** KNX, WebSocket and the AimX session were
  not measured at connector level; §5's per-connector rows will show them.

## 9. References

- [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md)
- [045 — Per-link codec selection](./045-per-link-codec-selection.md)
  (`with_serializer_into` scratch path)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, `pump_sink`, `StreamDialer`)
- `aimdb-bench/benches/b0_alloc_connector.rs`
