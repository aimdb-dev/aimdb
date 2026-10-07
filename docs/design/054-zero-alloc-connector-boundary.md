# 054 — Zero-allocation connector boundary

**Status:** 📝 Proposed, ready for implementation — pull model: each
connector's own task drives both directions; core runs no per-route tasks.
Revised 2026-10-03 after a throwaway prototype (§5.1), which met every allocation target and changed
the outbound pull into two steps (§4.2), the topic-writer builder (§4.3) and
the embedded write ring's sizing and wake-ups (§4.7). A second round made
the ready set part of the design (§4.2): polling every route was already
2.7 times slower than today's task per route at 8 routes. A third round
(2026-10-03, checked against the tree at `137ad2d`) made the ready set
lock-free (§4.2), covered the session client connectors and KNX (§4.8,
§4.9), fixed the embedded write-ring default (§4.7) and completed the
migration list (§6). Earlier revisions (per-route push pumps with
`RoutePublisher`, per-route record rings) are in this branch's history and
summarised in §7.

**Scope:** the per-message path between `aimdb-core` and connectors, in both
directions, and every in-tree connector. Breaking change to the connector
SPI and to two user-facing APIs: `with_topic_provider` is replaced by
`with_topic_writer` and its closure form `with_topic_fn` (§4.3), and
`KnxConnector::new` loses its `&'static Channels` argument (§4.9).
`link_from`, `link_to`, `with_deserializer`, `with_match_deserializer`,
`with_topic_resolver`, `with_serializer`, `with_serializer_into` and `Reader::recv` do not change;
`Reader` gains `poll_recv`.

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

**Connector-side copies and allocations** (read from code; §5.1 measures the
totals per round trip: 9 allocations on the embedded backend, 16 on the
native one):

| Backend | Inbound | Outbound |
|---|---|---|
| Native (`rumqttc`) | topic `publish.topic.clone()` + payload `Arc::from(publish.payload.as_ref())` | `destination.to_string()` + `payload.to_vec()`, required by `AsyncClient::publish`'s owned API |
| Embedded (`mountain-mqtt` codec, own session loop) | topic `topic_name.to_string()` + payload `Payload::from` | `destination.to_string()` + `payload.to_vec()` into `AimdbMqttAction::Publish`; then `session_loop::encode` allocates a `Vec<u8>` per packet for the `Channel<Vec<u8>, 4>` write queue |

**Structure today.** Every connector already owns at least one transport
task (MQTT event loop or session loop, KNX connection task, WebSocket
server, AimX session engine). Core adds one `pump_source` task per
connector and one `pump_sink` task per outbound route; the outbound pumps
all funnel into the same transport (one MQTT action channel, one KNX
command channel, one socket). The session client connectors
(`SessionClientConnector`, used by the UDS, TCP and serial clients, and the
WebSocket client) use `pump_client` instead: one task per outbound route
over `SerializedReader::recv` (the owned path, never `recv_into`) and one
per inbound subscription. They and the WebSocket server already call
`Router::route` with borrows instead of using `Source`.

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
  generic per-route or per-connector pump tasks (`pump_source`,
  `pump_sink`). Connectors that core itself implements (the session
  client and server connectors) own their tasks like any other connector.
  No compatibility layers.

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
- Allocations inside the AimX session engine. `ClientHandle::write` takes
  an owned topic and an `Arc<[u8]>` payload for its command channel; the
  session client connectors keep that API (§4.8) and are treated like
  `rumqttc` under G2.
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
pub struct OutboundRoutes { /* routes, ready set, scratch, staged message */ }

/// Dense index, `0..routes().len()`. A plain `usize`, so a connector indexes
/// its own per-route tables with it (`opts[msg.route.id]`).
pub type RouteId = usize;

pub struct RouteInfo {
    pub id: RouteId,
    pub default_topic: Arc<str>,     // from the link URL
    pub config: ConnectorConfig,     // qos, retain, record_index, …
    pub topic_capacity: usize,       // §4.3; 0 = default topic only
    pub payload_capacity: usize,     // `with_serializer_into`; 0 = owned only
}

pub struct OutboundMessage<'a> {
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

/// Values taken from a route's buffer, by outcome.
pub struct RouteStats { pub sent: u64, pub lagged: u64, pub topic_overflow: u64, pub serialize_failed: u64 }

impl OutboundRoutes {
    /// Subscribes every outbound link of `scheme`. Allocates the scratch once:
    /// the largest topic capacity plus the largest payload capacity.
    pub fn new(db: &AimDb, scheme: &str) -> DbResult<Self>;
    /// Routes, for parsing per-route configuration once at build.
    pub fn routes(&self) -> &[RouteInfo];
    pub fn stats(&self, id: RouteId) -> Option<RouteStats>;

    /// Take the next ready value from a route that woke, in FIFO order, and
    /// serialize it into the scratch. Lends nothing. `Ready(Some(id))`: a message from
    /// route `id` is staged. `Ready(None)`: every route is closed (final).
    pub fn poll_stage(&mut self, cx: &mut Context<'_>) -> Poll<Option<RouteId>>;
    /// Lend the staged message and clear it. `None` if nothing is staged.
    pub fn take_staged(&mut self) -> Option<OutboundMessage<'_>>;

    /// `poll_stage`, then `take_staged`, for hand-written `poll` code.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<OutboundMessage<'_>>>;
    /// `poll_fn(|cx| self.poll_stage(cx)).await?; self.take_staged()`.
    pub async fn next(&mut self) -> Option<OutboundMessage<'_>>;
}
```

**Why two steps.** A lending `poll_next` cannot be wrapped in `poll_fn`. An
`FnMut` closure cannot return a borrow of state it captured by `&mut`, since
it may be called again, so neither `next = poll_fn(|cx| self.poll_next(cx))`
nor a `poll_fn` select arm over it compiles ("captured variable cannot escape
`FnMut` closure body"). `poll_stage` returns no borrow and works in both
places; the message is lent once the poll has returned.

**Semantics of `poll_stage`.**

- If a message is already staged, its route is returned at once. A staged
  message that was never taken is neither lost nor overwritten.
- Otherwise it takes routes from its ready set in round-robin order and
  polls only those routes' typed readers (`poll_recv`, 037). The first
  value that is ready is serialized (topic writer, then serializer) into
  the scratch and staged.
- A value is taken from its buffer only in a call that returns `Ready`.
  `poll_stage` is therefore safe as a `select` arm: a losing arm takes
  nothing.
- After a value that is skipped (topic overflow, serializer error) or a
  `BufferLagged`, the same route is polled again until it returns `Pending`
  or a usable value, at most 32 times in a row. Moving on instead would
  leave that reader without a registered waker, and its next value would
  not wake the task. After 32 skips the route's own waker marks it for a
  later pass and wakes the task, so a route whose values all fail cannot
  hold the call while its producer keeps writing.
- Skips and lag are logged and counted per route (`RouteStats`); a closed
  buffer closes the route. None of these end the other routes.
- `Ready(None)` is final, and a connector with no outbound links gets it on
  the first poll. A `select` arm must be disarmed after it: an arm that
  resolves at once on every iteration keeps the loop from ever yielding
  (§4.7).
- `Pending` does not mean every route is empty. On Tokio a broadcast reader
  returns `Pending`, with a self-wake, once the task's cooperative budget
  (128 polls) is spent. An awaiting loop is unaffected; a synchronous "drain
  until `Pending`" must not read it as empty.
- One message is staged at a time (`&mut self`). The connector finishes with
  it (copies, encodes, or awaits its send) before staging the next.
- Cost of a wake-up: one `poll_recv` per route that woke, not per open
  route. Measured (§5.1) with one busy route among N and the producer in
  another task: 590–630 ns per message from 1 to 256 routes, against about
  470 ns for today's task per route and 450 ns (1 route) to 32,230 ns (256
  routes) for polling every route.

**Ready set.** Polling every open route on each wake-up costs about 120 ns
per idle route per message once the transport parks between messages (it
scans once to find the value and once to park again); at 8 routes that is
already 2.7 times slower than today's task per route (§5.1). So each route's
reader is polled with a waker of its own, and only routes that woke are
polled:

```rust
use portable_atomic::AtomicU32;           // already a core dependency
use futures_util::task::AtomicWaker;      // already a core dependency, no_std

struct Shared {
    ready: Box<[AtomicU32]>,   // one bit per route, ceil(routes / 32) words
    task: AtomicWaker,         // the transport task
}

struct RouteWake { id: RouteId, shared: Arc<Shared> }

impl Wake for RouteWake {
    fn wake_by_ref(self: &Arc<Self>) {
        self.shared.ready[self.id / 32].fetch_or(1 << (self.id % 32), Release);
        self.shared.task.wake();
    }
    fn wake(self: Arc<Self>) { self.wake_by_ref() }
}

// `ReadyRoutes` owns the `Arc<Shared>`, the route wakers, `cursor: RouteId`
// (plain field, `&mut self`) and `open: Box<[u32]>` (bitmap of routes that
// are not closed). `OutboundRoutes` holds one `ReadyRoutes`.
```

- **Lock-free, so a waker may fire from any context.** A route's waker is
  called by whoever produces into its record. On Embassy that can be a
  task on an `InterruptExecutor` (049) that preempts the transport task. A
  lock held by the transport task while the waker runs would deadlock a
  single core, which rules out the spin-locked FIFO the prototype used
  (§7, alternative 11). `fetch_or` and `AtomicWaker::wake` take no lock.
- **The transport task does not outrank its producers.** It runs at the
  same or a lower priority than every task or interrupt that writes its
  routes' records. `AtomicWaker::register`, called on every poll, answers
  a wake still in progress by waking the task again instead of waiting
  ("we simply schedule to come back later"). A transport that preempted
  that wake is polled again and again before the producer can finish it,
  and on one core the producer never does. Reproduced with the transport
  and a producer pinned to one CPU under `SCHED_FIFO`; the reverse order
  is fine. 049's deployment already satisfies the rule: cadences run above
  connector I/O.
- The route wakers are built once, in `OutboundRoutes::new`; polling a
  route uses `Context::from_waker(&route_waker)`. Every bit starts set,
  since no reader has registered a waker yet.
- **One loop, owned by the ready set.** `ReadyRoutes::poll_ready(cx,
  poll_route)` runs the whole scan below. `poll_stage` passes a closure that
  polls route `id`'s reader with the context it is handed and reports
  `Pending`, `Staged`, `Skipped` (skip or lag) or `Closed`. The orderings
  in this list are then kept in one tested function instead of by every
  caller; getting one wrong stalls a route for good, since a reader that
  returned a value keeps no waker. A skip or lag polls the same route
  again, since moving on would leave it with a clear bit and no waker, up
  to 32 times in a row; then the route's own waker sets its bit and wakes
  the task.
- `poll_ready` registers the task's waker before it reads the bitmap, so a
  route that wakes after the bitmap reads empty still wakes the task.
  Every wake takes the stored waker out, so registering after the scan
  would lose such a wake-up, not just risk a stale waker.
- **Clear, then poll.** `poll_ready` clears a route's bit
  (`fetch_and(!bit, Acquire)`) before polling its reader. A wake that lands
  during the poll sets the bit again, which costs at most one spurious
  re-poll and never loses a wake-up.
- **Round-robin.** The scan starts at the route after `cursor`, wraps once,
  and skips words that are zero. A route that staged a value has its bit
  set again (it may hold more, and its waker will not fire again for values
  already in its buffer), and `cursor` moves to it, so every other ready
  route is served before it is served again. A route that returns `Pending`
  stays clear until its waker fires; a closed route is removed from `open`
  for good, and `Ready(None)` follows the last one.
- Round-robin over ready routes is the fairness rule (§4.6).
- Cost per wake-up: one `poll_recv` per route that woke, plus a scan of
  `ceil(routes / 32)` words (8 at 256 routes).
- Nothing allocates per message: the bitmap is allocated once, and cloning
  a route waker is a reference-count increment.

The prototype measured a spin-locked FIFO (one `VecDeque` of route ids and
a queued flag per route), not this bitmap. Both poll only routes that woke,
so §5.1's flat 590–630 ns is expected to hold; the
`outbound_next_round_robin` and `outbound_next_parked` gates (§5) and the
informational 1/8/64/256-route bench confirm it during step 2 of §6.

**Where it is built.** In the connector's `build()`, not when its transport
task starts. Per-route configuration (§4.4) and frame sizes (§4.7) are then
validated before anything runs, and the value moves into the transport task.
Cursors start at build, so an SPMC ring keeps what is produced before the
task first polls instead of missing it.

**Naming.** `aimdb_core::Outbound` already names the session envelope, so
the pulled message is `OutboundMessage`.

**Thread-safety.** `OutboundRoutes: Send` (moved into a connector's
`Send` task; `&mut self` everywhere, so not `Sync`). `InboundDispatch:
Send + Sync + Clone`. Both hold only `Send` readers and `Send + Sync`
(de)serializers, as today's pumps do.

Removed from the public API: `pump_sink`, `SerializedReader`,
`SerializedSource`, `SerializedValue`, `SerializedValueInto`,
`SerializedPayload`, `RecvSerializedFuture` and `RecvSerializedIntoFuture`.
`AimDb::collect_outbound_routes` and `OutboundRoute` become crate-private;
their callers outside core (the embedded MQTT `warn_unsupported_qos`, and
the tests listed in §6) read `OutboundRoutes::routes()` instead. The typed
reader, topic writer and serializer stay inside core as the per-route state
of `OutboundRoutes`, behind a crate-private poll-shaped trait built by the
link's existing source factory (`SourceFactoryFn`, also crate-private), so
nothing is boxed or stored per message.

### 4.3 Outbound: topics are written, not returned

`TopicProvider` is replaced by:

```rust
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination into `out`. `Ok(false)` = use the link's default.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

// Closures implement it through a blanket impl.
impl<T, F> TopicWriter<T> for F
where
    F: Fn(&T, &mut TopicBuf<'_>) -> Result<bool, TopicOverflow> + Send + Sync,
{ /* … */ }

pub struct TopicOverflow;
// So that `write!(out, …)?` works inside a writer.
impl From<core::fmt::Error> for TopicOverflow { /* … */ }

// TopicBuf implements core::fmt::Write over the scratch topic region, plus
// push_str, len, capacity and as_str. A write past the route's capacity is
// refused whole, so the bytes are valid UTF-8 by construction.
```

- Two builder methods replace `.with_topic_provider(provider)`, the one
  user-facing break in this design:
  - `.with_topic_writer(capacity, writer)` takes any `TopicWriter<T>`, for
    reusable writer types.
  - `.with_topic_fn(capacity, |v, out| { write!(out, "sensors/{}/{}", v.site, v.id)?; Ok(true) })`
    takes a closure. It exists for type inference: under the generic
    `W: TopicWriter<T>` bound, that unannotated closure fails with E0282
    ("type annotations needed"). A parameter bound directly on the `Fn`
    signature gives the closure its higher-ranked signature.
- Overflow is detected by `TopicBuf`, not by the writer's return value. A
  writer that ignores the error (`let _ = write!(…); Ok(true)`) still has
  its value skipped. The value is skipped and counted per link
  (`RouteStats::topic_overflow`); the topic is never truncated.
- Every migrated provider needs a capacity: the longest topic the writer
  can produce. `TopicProvider` never had to say.
- Removes A5. Measured: 0 allocations with a written topic (§5.1).

### 4.4 Connector shape

A connector builds both objects in its `build()` (§4.2) and drives them from
its transport task. For a client with an async send, the outbound half is a
plain loop:

```rust
// In build(): a bad link fails the build.
let mut outbound = OutboundRoutes::new(&db, "mqtt")?;
let opts: Vec<PublishOpts> = outbound.routes().iter().map(PublishOpts::parse).collect::<Result<_, _>>()?;

// In the transport task:
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
- Configuration is parsed once per route at build; an invalid link fails
  the build instead of every publish. For MQTT that is a `qos` that is not
  0, 1 or 2, or a `retain` that is not `true` or `false`. Today a value
  that does not parse silently falls back to the default (QoS 1, no
  retain); after this change it is a build error, on both backends. The
  embedded backend keeps its once-per-route warning for `qos=2`, which it
  sends at QoS 1, now emitted from the same parse.
- `transport.rs` keeps `ConnectorConfig` (now `RouteInfo::config`, built by
  `ConnectorConfig::from_query` as today) and `PublishError` (KNX's
  `GroupWrite::try_new` returns it); only the `Connector` trait goes.
- A connector that must send while also reading (one socket, one task)
  puts `poll_stage` in its `select`, gated by its own readiness, takes the
  message with `take_staged` after the `select` returns, and disarms the arm
  after `Ready(None)` (§4.7).
- Shutdown: `next` returns `None` once every route is closed: at once when
  the scheme has no outbound links, otherwise when every outbound record's
  buffer has closed (the database is dropped). The connector flushes its
  transport as it sees fit.

Removed from the SPI: `Connector`, `Source`, `pump_source`, `pump_sink`,
the `Serialized*` types (§4.2) and `TopicProvider`. `pump_client` stays,
rewritten (§4.8). The `Vec<BoxFuture>` connectors return today shrinks to
their own transport tasks.

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
  last, an SPMC ring its backlog or a lag report. Confirmed on the Tokio
  buffers (§5.1): after five values with nobody pulling, single-latest sends
  `[4]`, a mailbox `[4]` and an SPMC ring `[0, 1, 2, 3, 4]`; ten values into
  a four-slot ring give one lag report, then `[6, 7, 8, 9]`. Document this
  per buffer type on the link builder.

  This holds where the connector owns its send path (embedded MQTT, the
  WebSocket server). The native MQTT backend hands each message to
  `rumqttc`, whose request channel (inbound topics + 10 deep) still queues:
  in the outage test (§5.1) it sent all ten values both before and after
  this change. Past that depth `publish` waits, and the rest stay in the
  record buffers.
- **One send at a time per connector.** Routes of one connector share the
  transport task, so a slow send delays all of them. That is already true
  underneath for every in-tree transport; the per-route pumps only hid it.
  A future transport with genuinely parallel sends spawns its own workers
  from its loop.
- **Fairness.** Round-robin over routes that woke (§4.2); after each
  message from a busy route every other ready route is served once before
  it, so it cannot starve the others. A route whose values keep failing to
  stage gives up its turn after 32 skips in a row, so it cannot either.
  Measured on the prototype's FIFO, and required of the bitmap by the same
  tests: three routes with three values each are pulled 0, 1, 2, 0, 1, 2,
  …; with 200 values queued on one route and one on another, the second is
  served first or second. Priorities are not in scope.
- **Embedded MQTT: outbound PUBLISH size is bounded.** Today `encode`
  allocates each packet at its exact length, so any size goes out. With the
  write ring (§4.7) a PUBLISH frame is at most `capacity / 2 −
  CONTROL_RESERVE`: 1,984 bytes at the 4,096-byte default. A route whose
  declared capacities exceed that fails the build; an owned-serializer
  payload over it is skipped, logged at warn level and counted by the
  connector. The ring size is a connector setting (§4.7).
- **Ingest and serialization run on the transport task.** Inbound
  deserialization already did for `pump_source`'s connectors; outbound
  serialization now does too. A slow user (de)serializer delays the
  transport's keep-alive and ack handling (§8).
- **Embedded MQTT: burst absorption.** Today the session loop parks on
  `events.send(event).await` when `pump_source` falls behind, so the
  `EventChannel` absorbs bursts and throttles the socket. With `dispatch`,
  a burst goes straight to the record buffers, which drop on overflow as
  they already do for a slow consumer. The loopback test for outbound
  progress under an inbound flood passes unchanged on the prototype.
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

*Outbound.* The session loop's action arm stages from `OutboundRoutes`,
under the same gate as today plus room in the write ring. Room is checked on
every poll of the arm, not once when it is built, because PUBACKs and pings
can take ring space while the arm is parked:

```rust
let action_ready = connected
    && !outbound_done                           // latched on Ready(None)
    && !state.waiting_for_responses()           // one in-flight slot
    && next_topic >= subscribe_topics.len();
let action_arm = poll_fn(|cx| {
    if !action_ready {
        return Poll::Pending;
    }
    if !ring.has_room(max_frame + CONTROL_RESERVE) {
        ring.room.register(cx.waker());
        if !ring.has_room(max_frame + CONTROL_RESERVE) {
            return Poll::Pending;
        }
    }
    match outbound.poll_stage(cx) {
        Poll::Ready(None) => { outbound_done = true; Poll::Pending }
        other => other,
    }
});
// after the select, on a staged route:
//   msg = outbound.take_staged()
//   packet = state.publish_packet(msg.topic, payload, qos[msg.route.id], retain[..])
//   ring.put(&packet, CONTROL_RESERVE).await   // the one copy
//   state.publish_update(&packet)
```

- `MqttSink`, `AimdbMqttAction::Publish` and the `ActionChannel` are
  removed. Subscriptions keep their current path (the loop already places
  them itself).
- **`Ready(None)` is latched.** A connector with only inbound links has no
  outbound routes. Passing `Ready(None)` through would resolve the arm on
  every iteration, and the session would never yield: in the prototype the
  idle-session test then saw no pings at all.
- **Write ring** (replaces `encode`'s `Vec` and `Channel<Vec<u8>, 4>`). A
  `bbqueue` 0.7 stream queue (`BBQueue<BoxedSlice, AtomicCoord, Polling>`),
  allocated once at build. The session loop writes every packet (CONNECT,
  SUBSCRIBE, PUBLISH, PUBACK, PINGREQ) with `grant_exact`, sizing it with
  `MqttLenWriter` and encoding with `MqttBufWriter` over the grant;
  `write_out` takes `read()` grants and releases them after `write_all`.
  Both ends run in the same task under the existing `select3`, so the ring
  needs no lock.
- **Wake-ups.** bbqueue's polling notifier wakes nothing, and `select3`
  polls `write_out` before the session loop, so bytes the loop commits
  would sit until something else woke the task. Two `AtomicWaker`s
  (embassy-sync) carry the signals: `data`, from a commit to `write_out`,
  and `room`, from a release to the loop.
- **`has_room` is a probe.** bbqueue 0.7 has no free-space query, so the
  gate takes `grant_exact(n)` and drops it uncommitted. A probe that wraps
  commits an early wraparound (the unused tail is skipped until the reader
  passes it); no data changes. A count of bytes in flight would not do: it
  says nothing about whether a contiguous grant exists.
- **Control reserve.** For a PUBLISH the loop takes `grant_exact(frame_len +
  CONTROL_RESERVE)` and commits only `frame_len`, so the next control packet
  finds room. The reserve covers one packet. A coalesced burst of QoS 1
  PUBLISHes needs a PUBACK each, so the PUBACK path waits for room instead
  of assuming it. PINGREQ stays lossy, as today.
- **PUBACK before the state update.** `receive_produce_response(&self)`
  returns a `Puback<'_>` that borrows the client state, so the PUBACK is
  encoded into the ring (after waiting for room, if it must) before
  `state.receive` takes `&mut`.
- **Reconnect.** At the start of each session the loop drains the write
  ring through its consumer, so no bytes from an old session reach a new
  socket ahead of its CONNECT. Unsent outbound values are still in their
  record buffers (§4.6).
- **Oversize.** A bipbuffer only grants contiguous space. Once its read and
  write pointers have both moved to `k`, an empty ring can grant at most
  `max(capacity − k, k − 1)`, about half its capacity in the worst case: a
  2,048-byte ring drained at offset 1,024 cannot grant 1,100 bytes. A larger
  frame could wait forever. So the largest frame a route can produce
  (`max_frame`: PUBLISH overhead + `max(default topic, topic_capacity)` +
  `payload_capacity`, from `RouteInfo`) plus `CONTROL_RESERVE` must fit in
  `capacity / 2`. A route that does not is rejected when the connector
  builds; an owned payload over the same bound is skipped and counted.
- **Sizes.** Default ring 4,096 bytes, the same as the existing
  `BUFFER_SIZE` the inbound side uses, and a 64-byte reserve: a PUBLISH of
  up to 1,984 bytes. The prototype used 2,048 bytes (960-byte cap);
  allocation counts do not depend on the size. The embedded builder gets
  `with_write_buffer(bytes)` to change it, checked at build against
  `2 × (max_frame + CONTROL_RESERVE)` of every route. The ring is
  allocated once per connector and reused across reconnects.
- `bbqueue = { version = "0.7", default-features = false, features =
  ["alloc"] }`, with `AtomicCoord`. Every embedded target this repository
  builds (`thumbv7em`, `thumbv8m.main`) has atomic CAS; `thumbv6m` is not
  a supported target (`aimdb-core` needs `alloc::sync::Arc`, which needs
  CAS).
- *Files.* Besides `session_loop.rs`, the embedded backend's `mod.rs`
  (`MqttSink`, `MqttSource`, `AimdbMqttEvent`, `AimdbMqttAction`, the
  channel types and `CHANNEL_SIZE`), `manager.rs` (the channel aliases),
  `session.rs` and `tls.rs` (both pass `events` and `actions` into
  `run_session`; they pass `&InboundDispatch` and `&mut OutboundRoutes`
  instead).

Cost per message: zero allocations and one copy of topic and payload
(scratch into the encoded frame), besides the network stack's own copy into
its TX buffer, which TLS needs anyway to encrypt. Measured (§5.1): a full
round trip went from 9 allocations (302 B) to 0, and on loopback TCP from
58.6 to about 49 µs. This also gives std users an allocation-free MQTT path
through `TokioTcpDialer` (052).

#### Native backend (`rumqttc`)

- *Inbound.* `dispatch(&publish.topic, &publish.payload)` in the event-loop
  task; `MqttEventLoopSource` is removed. This removes both the topic clone
  and the `Arc` payload copy.
- *Outbound.* One task runs the loop of §4.4 over `AsyncClient::publish`.
  `rumqttc` takes an owned `String` and payload (G2): the topic is copied,
  a borrowed payload is copied, an `OutboundPayload::Owned` `Vec` is moved
  in. `publish` waits for request-channel space, which is the backpressure.
  Per-route `qos` and `retain` are parsed at build.
- `rumqttc`'s own allocations (building `Publish`, its request channel) are
  outside G1. Measured (§5.1): a round trip went from 16 allocations
  (2,406 B) to 11 (557 B); what remains is the owned topic and payload and
  `rumqttc`'s own.

### 4.8 Session client connectors (`pump_client`)

`SessionClientConnector` (the UDS, TCP and serial clients) and the
WebSocket client both call `pump_client`. It stays public, with a new
signature:

```rust
pub fn pump_client(
    db: &AimDb,
    scheme: &str,
    inbound: InboundDispatch,
    handle: &ClientHandle,
) -> DbResult<Vec<BoxFut<'static, ()>>>;   // fallible: OutboundRoutes::new
```

- *Outbound.* One task, replacing one per route:

  ```rust
  while let Some(msg) = outbound.next().await {
      let payload: Payload = match msg.payload {
          OutboundPayload::Borrowed(b) => Payload::from(b),
          OutboundPayload::Owned(v) => Payload::from(v),
      };
      if handle.write(msg.topic, payload).is_err() {
          break; // engine stopped: every handle is gone
      }
  }
  ```

  `ClientHandle::write` is synchronous: it enqueues onto the engine's
  bounded command queue (`max_offline_queue`) with `force_send`, which
  displaces the oldest queued command when the queue is full. That is
  today's behaviour and does not change; like `rumqttc`'s request channel
  (§4.6), the engine's queue, not the record buffer, decides what survives
  an outage. The owned topic `String` and the `Arc<[u8]>` payload stay
  (non-goal, §2). The path moves from `recv` (owned serializer every time)
  to the scratch path, so `with_serializer_into` links no longer allocate a
  `Vec` first.
- *Inbound.* One task per subscription stays: each `ClientHandle::subscribe`
  stream is its own channel, and merging them is out of scope. Each task
  calls `inbound.dispatch(id, &update.data)` where it calls `router.route`
  today. These are the connector's own tasks (G4).
- *Callers.* `SessionClientConnector::build` and the WebSocket client's
  `build` change one line each (`InboundDispatch::new` and `?` on
  `pump_client`). The UDS, TCP and serial crates do not change.
- The session *server* side (`SessionServerConnector`, `serve`,
  `AimxDispatch`) does not use links and does not change.

### 4.9 KNX connector

Today the connection task talks to core through two `'static` channels
the application supplies (`Channels<N>`: telegrams in, `GroupWrite`
commands out), drained and filled by `pump_source` and one `pump_sink` per
route. With both pumps gone, nothing is left on the other end of either
channel, so both go:

- `Channels`, `TelegramChannel`, `CommandChannel` and `DEFAULT_QUEUE` are
  removed, as is the `N` parameter of `KnxConnector`.
  `KnxConnector::new(binder, delay, gateway_url)` drops its `channels`
  argument; the two examples drop their `static KNX_CHANNELS`. This is the
  second user-facing break (Scope).
- *Inbound.* `TelegramSink::try_send(String, Payload) -> bool` becomes
  `deliver(&self, topic: &str, payload: &[u8])`, implemented over
  `InboundDispatch`. `NeutralIo::forward` formats the group address into a
  stack buffer (`core::fmt::Write` over `[u8; 16]`; the longest address,
  `31/7/255`, is 8 bytes) instead of `addr.to_string()`. The `Vec<u8>` the
  tunnel engine hands to `forward` is the engine's own and stays (§8).
- *Outbound.* `CommandSource` is removed. `drive_connection` takes `&mut
  OutboundRoutes`, and `cmd_arm` becomes a `poll_fn` over `poll_stage`,
  armed only while connected, with `Ready(None)` latched as in §4.7.
  After the `select`, `take_staged` → `GroupWrite::try_new(msg.topic,
  payload)` → `engine.handle_command`. An invalid group address or an
  oversize payload is logged, counted by the connector and skipped, as
  `KnxSink::publish` rejects it today. The arm-order swap that keeps
  inbound traffic from starving commands stays.
- *Behaviour.* Commands produced while the tunnel is connecting or backing
  off wait in their record buffers (§4.6) instead of the 32-deep command
  channel.
- *Tests.* The task-count tests expect one future (the connection task)
  with or without outbound routes.

## 5. Measurement and gate

`b0_alloc_connector` is rewritten against the new interfaces. Targets, and
what the prototype measured (§5.1):

| Row | Before | After | Prototype |
|---|---|---|---|
| `inbound_dispatch` (was `inbound_route`) | 0 | 0 | 0 |
| `inbound_dispatch_pattern` | 0 | 0 | 0 |
| `inbound_dispatch_keyed_known` | 0 | 0 | 0 |
| `inbound_dispatch_keyed_new` | 1 | 1 (by design, 055 §5.6) | 1 |
| `inbound_pump_source_minimal` | 2 | removed (`Source` is gone) | — |
| `outbound_next_static_topic` (was `outbound_scratch_static_topic`) | 2 | 0 | 0 |
| `outbound_next_written_topic` (replaces the `TopicProvider` row) | 3 | 0 | 0 |
| `outbound_next_owned` (was `outbound_owned_static_topic`) | 3 | 1 (A6, by choice of serializer) | 1 |
| `outbound_next_round_robin` (new; 8 routes, all ready) | — | 0 | 0 |
| `outbound_next_parked` (new; every pull parks and is woken) | — | 0 | 0 |

Only `outbound_next_parked` parks before each pull, which puts the waker
path in the measured window. The prototype measured the round-robin and
parked rows both with polling every route and with the ready set (§4.2):
0 either way.

Beside the gated rows, an informational bench times one busy route among
1, 8, 64 and 256 (§5.1), so a change that brings back a per-route scan
shows up even though it allocates nothing. It is not a gate: timings need a
quiet runner.

The bench asserts exact values, so every change updates `EXPECTED` and the
baseline together. It compares the rounded per-message figure: a one-off
allocation can land in a 2,000-message window (one was seen in the
prototype) without being a per-message cost.

**Connector-level rows (G2).** A new integration test,
`aimdb-mqtt-connector/tests/alloc_round_trip.rs`, runs §5.1's round trip
(produce → PUBLISH QoS 1 → echo → dispatch → `recv`) over loopback TCP
against the echo broker in `tests/common`. It counts only allocations made
on the database thread (a counting global allocator with a thread-local
filter) over 300 round trips after 100 of warm-up. It runs under the
existing `_test-backend-parity` feature, which builds both backends, from a
new `make test` line beside `backend_parity`, so CI's `make test` gates it.

| Row | Asserted |
|---|---|
| embedded, allocations per round trip | 0 |
| native, allocations per round trip | ≤ 11 (the prototype's figure; `rumqttc`'s share can only be bounded) |

The test also prints bytes per round trip and the embedded backend's live
heap, and records the one outbound copy (scratch into frame) in a comment
beside the embedded row. Allocation counts do not depend on socket
options; latency does (§5.1), so latency is printed, not asserted.

**Gate.** A Makefile target `bench-gate` runs `cargo bench -p aimdb-bench
--bench b0_alloc_connector` (host only, a few seconds) and fails on any
assertion. A `bench-gate` job in `.github/workflows/ci.yml` runs it and is
added to `comprehensive-check`'s `needs`. It lands first, against today's
interfaces (§6 step 1), and is a required check from then on. It is the
counting-allocator bench, so its results are deterministic and do not need
a quiet runner. The informational timing bench is not part of the job.

### 5.1 Prototype

A throwaway prototype (not merged) built §4 against `main` at `dfe6adc`:
`InboundDispatch`, `OutboundRoutes` and `TopicWriter` in core beside the
old SPI, both MQTT backends and the WebSocket server's outbound path on the
new interfaces, and the embedded write ring. KNX, the WebSocket client, the
AimX session client and the Embassy adapter stayed on the old SPI, which
was not deleted, and no CI job was added.

Connector round trip: produce, outbound PUBLISH at QoS 1, the broker's
PUBACK and echo PUBLISH, inbound dispatch, the client's PUBACK, reader
`recv`. 300 round trips after 100 of warm-up, same harness on both trees;
latency as the median and range of 5 runs. Live heap is what the database
thread holds before the measured window; the peak is what the window adds.

| Backend | `main` | Prototype |
|---|---|---|
| Embedded: allocations | 9 (302 B) | 0 |
| Embedded: latency | 56.1 µs (49.3–58.3) | 44.4 µs (44.0–46.1) |
| Embedded: live heap / window peak | 41,346 B / +73 B | 45,646 B / +0 B |
| Native: allocations | 16 (2,406 B) | 11 (557 B) |

The write ring (2 KB) is most of the 4.3 KB the embedded backend now holds
up front; in exchange nothing is allocated while traffic flows. Nine
allocations at 14–23 ns each (host glibc) account for about 0.2 µs of the
12 µs latency gain; the rest is the removed task and channel hand-offs.
Timings come from one shared host. The embedded runs set `TCP_NODELAY` on
the dialer; nothing sets it for `rumqttc`, so the native round trip sits on
the 40 ms Nagle and delayed-ACK floor on both trees (§8).

Outage: ten values into a single-latest record while the broker accepted
the connection but did not answer, then the broker answered. Received:

| Backend | `main` | Prototype |
|---|---|---|
| Embedded | `0, 1, … 9` | `9` |
| Native | `0, 1, … 9` | `0, 1, … 9` |

Wake-up cost: one busy route among N idle ones, the producer and the
transport in separate Tokio tasks, so every message pays one wake-up. ns
per message, median of 5 runs:

| Routes | Task per route (`main`) | One task, poll every route | One task, ready set |
|---|---|---|---|
| 1 | 478 | 450 | 622 |
| 8 | 467 | 1,273 | 615 |
| 64 | 454 | 7,916 | 590 |
| 256 | 489 | 32,230 | 631 |

In a single task with a value always ready (no parking), polling every
route cost about 80 ns per idle route, and replacing today's two-allocation
path with `next` saved 34 ns per message (164 → 130 ns).

Behaviour covered by tests on real Tokio buffers, run against both polling
every route and the ready set: round-robin order, a hot
route not starving a quiet one, written and default topics, topic overflow
(including a writer that ignores the error), the owned-serializer fallback,
lag, outage semantics per buffer type (§4.6), a dropped pending `next()`,
500 values pulled under a `select` that loses every third poll, and a
skipped value not losing the wake-up. The existing MQTT loopback, TLS,
reconnect and QoS 1 flood tests and the WebSocket end-to-end tests pass on
the new paths.

## 6. Migration

| Who | Change |
|---|---|
| Application code | `.with_topic_provider(p)` → `.with_topic_writer(capacity, w)` for a writer type, or `.with_topic_fn(capacity, closure)`. Choose `capacity` as the longest topic the writer produces. KNX: `KnxConnector::new(binder, delay, url)` without the `Channels` argument, and drop the `static` that held it. An MQTT `qos`/`retain` value that does not parse now fails the build (§4.4). |
| Connector authors | Build `InboundDispatch` and `OutboundRoutes` in `build()` and move them into the transport task; call `dispatch` on receive; pull with `next`, or `poll_stage` + `take_staged` in a `select`, when the transport can send; stop on `None`. `Source`, `pump_source`, `pump_sink`, `Connector`, the `Serialized*` types and `collect_outbound_routes` are gone; session clients call the new `pump_client` (§4.8). |

Without adapters the core change breaks every connector at once, so the
work lands as one change (a feature branch merged once):

| Crate | Inbound | Outbound |
|---|---|---|
| `aimdb-core` | `InboundDispatch` (`router.rs`, `builder.rs`); remove `Source`, `pump_source` (`session/pump.rs`, `session/mod.rs`), public `inbound_router`; `Router` crate-private | `OutboundRoutes`, `RouteInfo`, `OutboundMessage`, `OutboundPayload`, `RouteStats`, ready set (§4.2); `TopicWriter`, `TopicBuf`, `TopicOverflow`, `with_topic_writer`/`with_topic_fn` (`typed_api.rs`, `connector.rs`); `Reader::poll_recv` (`buffer/reader.rs`); remove `pump_sink`, `Connector` (`transport.rs`), the `Serialized*` types, `TopicProvider`; `collect_outbound_routes`/`OutboundRoute` crate-private |
| `aimdb-core` session client | `pump_client` and `inbound_pump` take `InboundDispatch` (`session/client.rs`) | `pump_client` outbound becomes one task over `OutboundRoutes` (§4.8); `SessionClientConnector::build` (`session/connector.rs`) |
| `aimdb-mqtt-connector` embedded | `dispatch` in `drain_packets`; remove `MqttSource`, `AimdbMqttEvent`, `EventChannel`, `Received::Event` | action arm pulls from `OutboundRoutes`; `bbqueue` write ring; `with_write_buffer`; remove `MqttSink`, `AimdbMqttAction::Publish`, `ActionChannel`, `CHANNEL_SIZE`; `warn_unsupported_qos` reads `RouteInfo`. Files: `session_loop.rs`, `mod.rs`, `manager.rs`, `session.rs`, `tls.rs` (§4.7) |
| `aimdb-mqtt-connector` native | `dispatch` in the event-loop task; remove `MqttEventLoopSource` | publish loop task over `AsyncClient`; remove `MqttSink`; `qos`/`retain` parsed at build (`native.rs`) |
| `aimdb-knx-connector` | `TelegramSink::deliver` over `InboundDispatch`; remove `KnxSource`, `TelegramChannel` | `cmd_arm` pulls via `poll_stage`; remove `KnxSink`, `CommandSource`, `CommandChannel`, `Channels`, `DEFAULT_QUEUE`, `N` (`connector.rs`, `client.rs`, `lib.rs`; §4.9) |
| `aimdb-websocket-connector` | server: `WsDispatch` holds `InboundDispatch` (`dispatch.rs`, `builder.rs`); client: through `pump_client` | server: one broadcast loop pulls (`record_index` from `RouteInfo.config`), replacing `WsBusSink` (`server/connector.rs`); client: through `pump_client` (`client/builder.rs`) |
| `aimdb-uds-connector`, `aimdb-tcp-connector`, `aimdb-serial-connector` | — (via `SessionClientConnector`) | — |
| `aimdb-embassy-adapter` | remove `EmbassySourceRaw`, `EmbassySource` (`connectors.rs`) | remove `EmbassySinkRaw`, `EmbassySink` (`connectors.rs`); update the module docs in `connectors.rs` and `send_wrapper.rs` that name the pumps |
| examples | — | `tokio-knx-connector-demo`, `embassy-knx-connector-demo`: drop `Channels` |
| tests | `aimdb-data-contracts/src/link_codec.rs` (inbound half), `aimdb-mqtt-connector/tests/link_ext_tests.rs`, core's `typed_api.rs` router tests (`pump_source_routes_through_the_given_router` is removed) → `InboundDispatch` | `link_codec.rs` outbound half and `link_ext_tests.rs` → `OutboundRoutes::routes()` and `next`; `aimdb-mqtt-connector/tests/session_loop.rs` (`warn_unsupported_qos` test) → `RouteInfo`; `topic_provider_tests.rs` (mqtt, knx), websocket `e2e.rs` and `decouple_record_keys_topics.rs` → `TopicWriter`; core's `fused_reader_*` tests in `typed_api.rs` → the same cases against `OutboundRoutes` (buffer errors, serializer skips, scratch and owned fallback, invalid length, written topic); KNX task-count tests expect one future; `aimdb-client/tests/pump_client.rs` → new `pump_client` signature; new `alloc_round_trip.rs` (§5) |
| bench, docs | — | `b0_alloc_connector` rewritten and baseline replaced (§5); `aimdb-bench/README.md`; `CHANGELOG.md` entries in core, mqtt, knx, websocket, embassy-adapter |

**Order:**

1. On main, independently: the `bench-gate` job for today's bench (§5),
   and the native MQTT topic move (`publish.topic` instead of `.clone()`).
2. On the feature branch: `Reader::poll_recv`, `InboundDispatch`,
   `OutboundRoutes` with the ready set, `TopicWriter`; bench rewritten.
   Confirm the round-robin, parked and 1/8/64/256-route numbers (§4.2)
   before any connector moves.
3. MQTT embedded in two steps, each tested with the loopback harness:
   1. the `bbqueue` write ring, replacing `encode`'s `Vec` and the
      `Vec` channel (touches `perform`, `drain_packets`, `queue`,
      `queue_lossy`, `write_out`);
   2. the action arm pulls from `OutboundRoutes`; `ActionChannel` and
      `MqttSink` removed.
4. MQTT native, KNX, WebSocket, `pump_client`, Embassy adapter, examples,
   tests; `alloc_round_trip.rs`.
5. Merge once the gate and every connector's tests pass.

The prototype supports landing it as one change. While `with_topic_provider`
still existed but the pull path ignored it, ten WebSocket tests timed out
instead of failing to compile. Removing the old SPI in the same change makes
every unmigrated caller a compile error.

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
8. **A lending `poll_next` as the only pull (previous revision).** One call
   instead of two, but it cannot be wrapped in `poll_fn`: an `FnMut` closure
   cannot return a borrow of state it captured by `&mut`. Neither `next()`
   nor a `select` arm could use it, so the prototype split it into
   `poll_stage` and `take_staged` (§4.2).
9. **Embedded: gate on a count of bytes in flight.** Cheaper than a probe
   grant, but a bipbuffer's free bytes say nothing about whether a
   contiguous grant of that size exists. The probe answers the question the
   gate asks (§4.7).
10. **Poll every route on each wake-up (previous revision).** No per-route
    wakers and the cheapest option with one route (450 ns against 622 ns),
    but about 120 ns per idle route per message: 1,273 ns at 8 routes and
    32,230 ns at 256, where today's task per route stays near 470 ns. The
    ready set stays flat at 590–630 ns (§5.1).
11. **Ready set as a spin-locked FIFO (the prototype).** A `spin::Mutex`
    around a `VecDeque` of route ids, with a queued flag per route. Strict
    oldest-first order, and what §5.1 measured. But route wakers run in the
    producer's context, and on Embassy that can be an `InterruptExecutor`
    (049) preempting the transport task while it holds the lock, which
    deadlocks a single core. The bitmap (§4.2) takes no lock and gives
    round-robin order instead, which meets the same fairness tests.
12. **Keep KNX's `Channels` with a core-side pump into them.** Keeps
    `KnxConnector::new` unchanged, but brings back the per-route task and
    the queue between the record buffers and the transport that this
    design removes everywhere else (§4.9).
13. **`futures-util`'s `SelectAll` or `FuturesUnordered` as the ready
    set.** Both give each child its own waker and queue the ones that woke,
    which is what §4.2 needs. But `SelectAll` puts a stream back after
    every item, and each insertion allocates a task node: one allocation
    per message (10,000 for 10,000 items with futures-util 0.3.33), which
    the `outbound_next_*` rows (§5) forbid. `FuturesUnordered` alone holds
    futures that finish once, so a route would be re-inserted after every
    value too. And routes read different value types into one scratch
    buffer passed in at poll time, which a `Stream` cannot take. (Its
    queue can be seen half-updated by a task that preempted a producer
    mid-insertion, which then wakes itself; `AtomicWaker::register` does
    the same, which is why §4.2 keeps the transport from outranking its
    producers. It is not a reason to prefer one over the other.)
14. **`futures-concurrency`'s `Merge` over a `Vec` of streams.** The
    closest match: a waker per child, a readiness bitset, a parent waker,
    and no allocation per item. But with `std` the readiness sits behind a
    `std::sync::Mutex` that every wake takes, and the `no_std` build gives
    every child the parent waker and polls all of them: 384 child polls
    per item with one busy stream among 256 (futures-concurrency 7.7.1),
    which is alternative 10. The scratch-buffer limit of 13 applies too.

## 8. Open questions

None of these blocks implementation. Each is a measurement or a follow-up;
§6 can proceed with the decisions above.

- **(De)serialization latency on the transport task.** Embedded: a slow
  user serializer or deserializer delays keep-alive and ack handling in the
  session loop. Native: it delays polling `rumqttc`'s event loop or the
  publish loop. Measure on the STM32H5 rig (037's B3) and with
  `alloc_round_trip.rs`'s printed latency; decide whether to document a
  budget. Measurement, after merge.
- **Ready set on an MCU.** The bitmap (§4.2) is confirmed on the host by
  §6 step 2. Its atomics and waker hops have not been measured on the
  STM32H5 rig, nor on Embassy's buffers. Measurement, after merge.
- **Outage semantics per buffer type.** Confirmed for the Tokio buffers
  (§4.6). Confirm for the Embassy buffers with the loopback harness in §6
  step 3; a difference is documented on the link builder, not designed
  around.
- **Other connectors' own copies.** KNX's tunnel engine hands `forward` an
  owned `Vec<u8>` per telegram, and the AimX session engine takes owned
  topics and payloads (§4.8). Both are outside G1; a follow-up can make
  either borrow.
- **Inbound chunk channel (embedded).** The `read_into` side (`Channel<Chunk,
  1>` into `PacketReader`) could read socket bytes straight into a `bbqueue`
  grant. Out of scope; noted for a follow-up.
- **`TCP_NODELAY` (not part of this design).** Neither `TokioNet::tcp()` nor
  the native backend's `rumqttc` options set it. With QoS 1 traffic in both
  directions, every exchange then waits on the Nagle and delayed-ACK
  interaction, about 40 ms per round trip (§5.1). Track separately.

Resolved in the 2026-10-03 revision: write-ring sizing (4,096-byte default,
`with_write_buffer`; §4.7) and `thumbv6m` (not a supported target; §4.7).

## 9. References

- [037 — Zero-allocation consume path](./037-zero-alloc-consume-path.md)
- [045 — Per-link codec selection](./045-per-link-codec-selection.md)
  (`with_serializer_into` scratch path)
- [049 — Real-time cadence](./049-real-time-cadence.md)
  (`InterruptExecutor` producers, §4.2)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, `pump_sink`, `StreamDialer`)
- [053 — Zenoh connector](./053-zenoh-connector.md)
- [055 — Wildcard inbound links](./055-wildcard-inbound-links.md)
  (grammar, keys, `TopicMatch`, §7 relation to this design)
- [`bbqueue`](https://github.com/jamesmunns/bbqueue) 0.7
- `embassy_sync::waitqueue::AtomicWaker` (write-ring wake-ups, §4.7)
- `aimdb-bench/benches/b0_alloc_connector.rs`
- `aimdb-mqtt-connector/src/embedded/session_loop.rs` (`client_loop`,
  `drain_packets`, `perform`, `encode`, `write_out`)
