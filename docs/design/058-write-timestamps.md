# 058 — Write timestamps

**Status:** ✅ Implemented

**Scope:** [#326](https://github.com/aimdb-dev/aimdb/issues/326), parts A and
B. A mandatory write timestamp on every record value in `aimdb-core`, taken
from one monotonic clock per instance and carried in a stable value envelope
(`Stamped<T>` with opaque `WriteMetadata`) that later write metadata can
grow behind; its exposure through readers,
latest-value access and join triggers; AimX protocol 4.0, which carries it on
every value; and `aimdb-client`, `aimdb` CLI and `aimdb-mcp` results. Breaking
for Rust APIs and the wire.

---

## 1. The rule

> Every successful record write receives the current timestamp from the
> local AimDB instance's monotonic clock. Timestamps are never inherited or
> imported.

> A stamp is nanoseconds since this instance started. Stamps are scoped to
> one instance lifetime; a client must reset timestamp comparisons when it
> reconnects.

> `Stamped<T>` is the stable envelope for a record write. Metadata lives in
> a concrete `WriteMetadata` type with private fields and accessor methods,
> is created only by `RecordWriter`, and travels with the value through
> metadata-preserving conversions. Wire encoding picks fields out of it
> explicitly.

A timestamp means **"this value was written into this instance at this
time."**

- It is monotonic local time since the instance started (`build()`), not
  Unix wall-clock time.
- A source output is stamped when the source writes it.
- A transform or join output is stamped when its output is written, not
  with the time of its inputs.
- An inbound connector value is stamped when it is written locally, not with
  any time the remote side sent.
- A remote `record.set` is stamped when the write is applied.

What a write timestamp does **not** say:

- When a sensor measured. Measurement time, remote origin time and
  sampling-cycle identifiers belong in the application's data contract as
  explicit fields (as `Settable::set(value, timestamp)` already does) when an
  application needs them.
- Which values belong to the same cycle, or what caused what. Two values
  written close together are not thereby a pair, and a later stamp is not
  thereby an effect. Pairing on write time is a heuristic the application
  chooses knowingly.

## 2. Requirements

| # | Requirement |
|---|---|
| R1 | Every value handed to a record's buffer is stamped there. There is no optional, sentinel or unstamped path. |
| R2 | One clock per instance. A stamp does not depend on which thread, task or runtime performs the write. |
| R3 | Stamps of one writer never decrease. Equal stamps are allowed. |
| R4 | No allocation added. One clock read per write. |
| R5 | Tokio, Embassy and WASM. Under Tokio paused time, stamps follow `tokio::time` exactly, so simulations are reproducible. |
| R6 | Timestamps are part of every read result, in process and on the wire. No stamped/unstamped variants of an API. |

**Non-goals:**

- wall-clock stamps
- clock synchronisation between instances
- importing a peer's time over a link
- join pairing
- an instance-generation identifier, so a client can tell a restart from a
  reconnect; deferred to reconnect-aware caching or resumable subscriptions
- estimating a value's age against the server's current time from outside
  the instance

## 3. Clock

### 3.1 Types

```rust
/// Nanoseconds since the instance started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(u64);

impl Stamp {
    pub const fn from_nanos(n: u64) -> Self;
    pub const fn as_nanos(self) -> u64;
    /// `self − earlier`, saturating at zero.
    pub fn since(self, earlier: Stamp) -> core::time::Duration;
}

/// The instance's clock, created once by `build()`.
pub trait InstanceClock: Send + Sync {
    /// Monotonic, instance-relative: time since instance start.
    /// Nondecreasing (consecutive readings may be equal); saturates at
    /// `u64::MAX`.
    fn now(&self) -> Stamp;
    /// Absolute wall-clock time since the Unix epoch. Independent of `now()`'s
    /// origin; may be adjusted, and is `None` without a wall clock.
    fn unix_time(&self) -> Option<(u64, u32)>;
}
```

**Time reads belong to the clock.** The clock carries both time sources an
instance has, each with its own guarantee: `now()` is monotonic and relative
to instance start; `unix_time()` is absolute, may be adjusted by clock
corrections, and may be unavailable. Unix time is never derived from a stamp.

`RuntimeOps` no longer reads time. It keeps identity, sleep and logging, and
gains a clock factory; `now_nanos()` and `unix_time()` are removed:

```rust
pub trait RuntimeOps: Send + Sync {
    fn name(&self) -> &'static str;
    /// A new clock whose monotonic origin is the moment of this call.
    fn new_clock(&self) -> Arc<dyn InstanceClock>;
    fn sleep(&self, d: Duration) -> BoxFuture;
    fn log(&self, level: LogLevel, msg: &str);
}
```

`new_clock()` is infallible. Only the adapter knows how to build a clock
(a Tokio handle, an Embassy `Instant`, `performance.now()`), and the origin
must be the instance start, not the adapter's construction, so the adapter
provides a factory rather than a clock of its own.

**One notion of time per instance.** `build()` calls `new_clock()` once and
keeps the result. `RuntimeContext` holds the runtime and a clock
(`{ ops, clock }`); every context the instance hands out
(`AimDb::runtime_ctx()`, services, connectors, inbound dispatch, outbound
routes) carries the instance clock, so connectors, session idle timers and
`ctx.time()` read the clock that stamps writes (R2). `ctx.time().now()`
returns a `Stamp`; `ctx.time().unix_time()` reads the clock's wall time.
Engines that read time, such as the session client engine (`run_client`),
take a `RuntimeContext`; connectors that only read time take the
`Arc<dyn InstanceClock>`. Code outside an instance (a standalone client)
creates its own clock with `RuntimeContext::new(ops)`. Profiling reads the
instance clock directly and has no origin of its own.

### 3.2 Per adapter

The origin is instance start on every adapter, captured in `build()`.

| Adapter | `now()` | Origin | `unix_time()` |
|---|---|---|---|
| Tokio | `tokio::time::Instant::now()` in the captured runtime; `std::time::Instant` if created outside a runtime | that source's `Instant` at `build()` | `SystemTime` |
| Embassy | `embassy_time::Instant::now()` (µs × 1000, saturating) | that `Instant` at `build()`, not boot | boot anchor from `set_unix_time` plus uptime; `None` until set |
| WASM | `performance.now()` | `performance.now()` at `build()` | `Date.now()` |

**Tokio.** A clock must not depend on which runtime reads it: a write from
outside the runtime, such as `aimdb-sync`'s `SyncProducer::set`, which runs
on the caller's thread, or a write from another runtime, would otherwise read
a different origin and, under `test-util`, a different clock. The clock
uses `Handle::try_current()` at creation to choose a fixed source:

```rust
enum Source {
    Tokio { handle: Handle, origin: tokio::time::Instant },
    System { origin: std::time::Instant },
}
```

When a runtime is current, the clock captures its handle and a Tokio origin.
For that source, `now()` reads:

```rust
let now = if Handle::try_current().map(|h| h.id()).ok() == Some(handle.id()) {
    Instant::now()
} else {
    let _guard = handle.enter();   // selects the captured runtime's clock
    Instant::now()
};
let nanos = now.saturating_duration_since(*origin).as_nanos();
Stamp(u64::try_from(nanos).unwrap_or(u64::MAX))
```

The fast path costs one thread-local read; writes from foreign threads pay a
context enter. With `tokio::time::pause()`, every write reads the paused
clock of the instance's runtime, wherever it comes from (R5).

Created outside a runtime, the clock uses `std::time::Instant` for both its
origin and every reading: **created outside a runtime → real time**. It ignores
Tokio's paused time even when later read inside a runtime. This keeps creation
infallible and readings monotonic across runtime boundaries; there is no
captured runtime whose paused time it could follow. An async `build()` may be
driven by another executor, and standalone contexts may be created in
synchronous code, so both sources are supported.

The per-runtime anchor `TokioAdapter::now_nanos()` used (#333) goes away
with `now_nanos()`: each clock has its own origin, so a paused runtime that
advanced earlier cannot shift a later clock.

**Embassy.** `embassy_time::Instant` is global, so it is already one clock;
the instance clock subtracts the `Instant` captured at `build()`.

**WASM.** One instance lives in one JS realm, and `performance.now()` is
that realm's clock. The `f64` milliseconds convert to nanoseconds with the
same clamp to `0..=u64::MAX`.

### 3.3 Binding in `build()`

`build()` runs in this order:

1. Aggregated validation of the configuration, unchanged.
2. Bind the runtime and create the clock (`new_clock()`).
3. Bind the clock to every record in a second pass over the record storages.
4. Construct the database and start it.

Every record has its clock before any task runs, so there is no fallback
clock and no unstamped window.

### 3.4 Order

Stamps of one writer never decrease (R3). When several writers write to one
record concurrently (clones of a `Producer` on a multi-threaded runtime), the
clock is read before the value enters the buffer, so buffer order can differ
from stamp order by the width of that race. Buffer order (and AimX `seq`)
remains the authority on order; the stamp is the authority on time.

## 4. Inside the database

### 4.1 The envelope

This migration moves every read result from a bare `T` to an envelope. That
move happens once; the envelope is shaped so that the next piece of write
metadata changes `WriteMetadata` and the write path, not every caller.

```rust
/// A record value together with the metadata of the write that stored it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamped<T> {
    pub value: T,
    metadata: WriteMetadata,
}

/// Metadata of one record write. Fields are private; read them through
/// accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteMetadata {
    stamp: Stamp,
}

impl WriteMetadata {
    /// Created by `RecordWriter` at write time.
    pub(crate) fn new(stamp: Stamp) -> Self;
    /// Metadata decoded from a peer's wire value (§6) or built in tests.
    /// Carries exactly what the wire carries; never reaches a buffer.
    pub fn from_wire_stamp(stamp: Stamp) -> Self;
    pub fn stamp(&self) -> Stamp;
}

impl<T> Stamped<T> {
    pub fn new(value: T, metadata: WriteMetadata) -> Self;
    pub fn metadata(&self) -> &WriteMetadata;
    pub fn stamp(&self) -> Stamp;           // self.metadata.stamp()
    pub fn into_value(self) -> T;

    // Same write, different representation: metadata is kept as a whole.
    pub fn as_ref(&self) -> Stamped<&T>;
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Stamped<U>;
    pub fn try_map<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<Stamped<U>, E>;
}
```

- **`value` is public, metadata is not.** Callers keep the convenience of
  `sample.value`. The private `metadata` field rules out struct literals and
  exhaustive destructuring outside `aimdb-core`, so neither `Stamped<T>` nor
  `WriteMetadata` locks callers into today's field layout. Readers use
  `.value` and `.stamp()`; a later field adds an accessor and changes no
  existing call.
- **Conversions keep all metadata.** `as_ref`, `map` and `try_map` are for
  representations of the *same* write: encoding to JSON or bytes, borrowing,
  narrowing. They copy `WriteMetadata` as a whole, so a field added later
  is carried along without revisiting the call site. Code inside the
  workspace never rebuilds an envelope from a value and a stamp.
- **New writes get new metadata.** A transform or join output, an inbound
  connector value and a `record.set` are new writes (§1). They pass a plain
  `T` to `RecordWriter`, which creates fresh metadata; there is no write API
  that accepts a `Stamped<T>` or a `WriteMetadata`, so an input's metadata
  cannot leak into an output.
- **One concrete type.** No generic metadata parameter on `Stamped`, no
  metadata map, no extension hooks: they would add type parameters to every
  record API and potentially allocations (R4) without a requirement that
  needs them (§10).
- **`WriteMetadata` is `Copy`.** That is a commitment: write metadata stays
  small, fixed-size and allocation-free. A field that cannot honour it would
  break R4 anyway and is a deliberate breaking change.

What the envelope does not make cheap: a new metadata field still grows every
buffer slot (§4.5), may need new inputs at write time, and, if it should
reach peers, a protocol decision (§5). It removes source churn, not those.

### 4.2 Storage

- Records store `Stamped<T>`: `TypedRecord<T>::buffer` is
  `Option<Arc<dyn DynBuffer<Stamped<T>>>>`, and the adapters' `.buffer(cfg)`
  build `TokioBuffer::<Stamped<T>>`, `EmbassyBuffer::<Stamped<T>, …>` and the
  WASM buffer the same way. The buffers already store their element inline
  and are generic over it; their implementations do not change.
- `RecordRegistrar::buffer_raw` / `buffer_with_cfg` take
  `Box<dyn DynBuffer<Stamped<T>>>`.

### 4.3 Writing

`RecordWriter::push(value)` reads `clock.now()` immediately before the
buffer push and pushes `Stamped::new(value, WriteMetadata::new(stamp))`. It
is the only place in the crate that creates `WriteMetadata` for a buffer; `try_push` keeps its
default delegation to `push`. This is the single path behind
`Producer::produce` / `try_produce`, single transforms, join outputs,
inbound connector dispatch and AimX `record.set`, so every writer in §1 is
covered by one call site. The write APIs keep their signatures; there is no
way to pass a stamp or metadata in.

No buffer refuses a write today, so every value handed to the buffer is
stamped there (R1). `TryProduceError<T>` keeps carrying a plain `T`: a value
it returns was never stored.

Values restored by `aimdb-persistence` on startup are writes like any
other and get the stamp of the restore.

### 4.4 Reading

The write metadata, and with it the stamp, is part of every read result:

| API | Before | After |
|---|---|---|
| `Reader<T>::recv` / `poll_recv` / `try_recv` | `T` | `Stamped<T>` |
| `TypedRecord::latest`, `RecordValue<T>` | value | value + `metadata()` / `stamp()` |
| `AimDb::latest::<T>` | `Option<T>` | `Option<Stamped<T>>` |
| `JoinTrigger` | `index`, `as_input` | + `metadata()` / `stamp()`: the input value's write metadata |
| `JsonBufferReader::poll_recv_json` / `try_recv_json` | `serde_json::Value` | `Stamped<serde_json::Value>` |
| `JsonBufferReader::poll_recv_json_bytes` / `try_recv_json_bytes` | `Vec<u8>` | `Stamped<Vec<u8>>` |
| `JsonReader::recv_json` / `try_recv_json` | `serde_json::Value` | `Stamped<serde_json::Value>` |
| `JsonReader::recv_json_bytes` / `try_recv_json_bytes` | `Vec<u8>` | `Stamped<Vec<u8>>` |
| `JsonRecordAccess::latest_json` | `Option<serde_json::Value>` | `Option<Stamped<serde_json::Value>>` |
| `JsonRecordAccess::latest_json_bytes` | `Option<Vec<u8>>` | `Option<Stamped<Vec<u8>>>` |
| `AimDb::try_latest_as_json` (and its inner counterpart) | `Option<serde_json::Value>` | `Option<Stamped<serde_json::Value>>` |
| `AimDb::try_latest_as_json_bytes` (and its inner counterpart) | `Option<Vec<u8>>` | `Option<Stamped<Vec<u8>>>` |
| `RecordValue::as_json` | `Option<serde_json::Value>` | unchanged: encodes the value of a `RecordValue` that already carries its metadata |
| `aimdb-sync` `get` / `try_get` / `get_latest` (and `_with_timeout`) | `T` | `Stamped<T>` |

`AimDb::now() -> Stamp` reads the instance clock, so in-process code can
compute a value's age (`db.now().since(v.stamp())`). `RuntimeContext`'s
`time().now()` reads the same clock (§3.1).

No `recv_stamped`/`recv` pair: callers that want only the value write
`.value`. The migration is mechanical and the compiler finds every site.

**Internal forwarding carries `WriteMetadata`, not a stamp.** Types that hand
a written value along inside the process hold the whole metadata, with a
`stamp()` convenience, rather than a timestamp field each:

- **`RecordValue<T>`** wraps a `Stamped<T>` (plus its codec) instead of a
  `T`, and gains `metadata()`, `stamp()` and `into_stamped()`; `get()`,
  `into_inner()` and `Deref` keep returning the value.
- **`JoinTrigger::Input`** gains `metadata: WriteMetadata`, filled by the
  input forwarder from the `Stamped<T>` it received, and `metadata()` /
  `stamp()` accessors. The variant becomes `#[non_exhaustive]`, so user
  `match` arms use `..` and the next field does not break them.
- **`OutboundMessage`** gains `metadata: WriteMetadata`, taken from the
  `Stamped<T>` the route serialized, and a `stamp()` accessor. It is the
  source of the WebSocket connector's `ts` (§5.1).

The next metadata field then reaches joins and outbound routes without
changing these types.

**Transforms.** A single transform's closure keeps receiving `&I`, the input
value without its metadata: the output is a new write with its own metadata,
so the input's has no use there. Only a join's `JoinTrigger` exposes it, for
applications that choose to pair on write time (§1).

**JSON read paths.** Every type-erased read returns the encoded payload
together with its write metadata, `Stamped<serde_json::Value>` or `Stamped<Vec<u8>>`,
so no JSON path can drop the stamp (R6):

- **One read, one conversion.** Payload and metadata come from the same
  `Stamped<T>` taken from the buffer (one `peek`, `poll_recv` or
  `try_recv`), then converted with the envelope's own methods:
  `sample.as_ref().try_map(|v| codec.encode(v))`. A path never reads the
  value and the stamp separately, which could pair a value with a newer
  write's stamp, and never rebuilds an envelope by hand, which would drop
  any field it does not name.
- **Codecs encode `T` only.** `RecordCodec` and the serializers are
  unchanged; metadata is not part of the record's JSON. The read adapter
  (`TypedRecord`'s `JsonRecordAccess` impl and its `JsonBufferReader`) carries
  it beside the payload, and the AimX layer builds the wire envelope (`ts`
  beside `data`, §5.1).
- **Default methods keep the stamp.** The trait defaults that derive bytes
  from a tree (`latest_json_bytes` from `latest_json`,
  `poll_recv_json_bytes` from `poll_recv_json`) convert with
  `try_map(|v| serde_json::to_vec(&v))`, keeping the metadata whole.
- **Consumers:** `record.get` and its ring fallback, `record.drain`, wildcard
  snapshots, subscription streams (`stream_record_updates`) and the MCP and
  WebSocket paths all take the stamp from these results (`.stamp()`), never
  from a second call. Outbound adaptation converts the same way.

### 4.5 Memory

`WriteMetadata` holds one `u64` today, so `size_of::<Stamped<T>>()` is
`size_of::<T>() + 8` rounded up to `max(align_of::<T>(), 8)`. Every field
added to `WriteMetadata` later is paid per slot in every buffer; the
`size_of` assertion below makes that visible. `u64` is 8-aligned on every target we ship,
Cortex-M and `wasm32` included.

| `T` | `size_of T` | `Stamped<T>` | Added per slot |
|---|---|---|---|
| `u8`, `bool` | 1 | 16 | 15 |
| `f32`, `u32` | 4 | 16 | 12 |
| `f64`, `u64` | 8 | 16 | 8 |
| 12 B, align 4 | 12 | 24 | 12 |
| 24 B, align 8 | 24 | 32 | 8 |

A slot grows by 8 to 15 bytes, padding included. Examples:

- 16-slot `EmbassyBuffer<f32>` ring: 64 → 256 bytes of element storage.
- `SingleLatest` / `Mailbox`: one slot, so 8 to 15 bytes per record.
- Tokio broadcast rings grow by the same amount per slot, on top of their
  existing per-slot bookkeeping.

The Embassy tests assert the slot size behind the default `CAP = 16` buffer
of an `f32` record (the buffer holds its slots behind one allocation), and
the existing `aimdb-bench` allocation gates stay in place, so a change to
the budget is visible in review. A formal before/after
`produce` → `recv` latency comparison is deferred. A packed slot
(`[u8; 8]` instead of `u64`) would save the padding for small `T` at the
cost of unaligned reads on every receive; not adopted.

## 5. AimX 4.0

The wire changes shape, so the major version goes up: `PROTOCOL_VERSION`
becomes `"4.0"`. Compatibility stays by major version: a 3.x client is
refused at `hello` with `VersionMismatch`, and the WebSocket `?v=` gate
refuses 3.x URLs the same way. 4.0 includes everything 3.1 has (057).

`WelcomeMessage` changes only in its `version`, and `instance.info` does not
change. Neither carries clock information: stamps are scoped to one instance
lifetime (§1), and a client resets its timestamp comparisons on reconnect.

**The wire stays explicit.** `WriteMetadata` is not serialized as a whole;
the AimX encoder takes `stamp()` and writes it as `ts`. A future metadata
field reaches the wire only through its own compatibility decision, so an
internal change never alters protocol messages by itself.

### 5.1 Values

`ts` is mandatory wherever a record value appears. It is the value's stamp
in nanoseconds, as a JSON integer.

| Message | 3.x | 4.0 |
|---|---|---|
| `event` | `{t, sub, seq, topic?, data}` | `{t, sub, seq, topic?, ts, data}` |
| `snap` | `{t, sub, seq, topic?, last?, data}` | `{t, sub, seq, topic?, last?, ts, data}` |
| `record.get` reply | `<value>` | `{"value": <value>, "ts": n}` |
| `record.drain` reply | `{record_name, values: [<value>…], count}` | `{record_name, values: [{"value": …, "ts": n}…], count}` |
| `record.set` reply | `{status, value}`, or `{status}` | `{status, value, ts}`, or `{status}` |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,"data":{"celsius":21.5}}
```

- `Outbound::Event` / `Snapshot` and `SubUpdate` gain `ts: Stamp`. These
  are protocol-layer types shared with the client, so they carry the wire
  field, not `WriteMetadata`; the codec's
  `Frame` gains `ts: Option<u64>` for decoding only, and a value frame
  without it is malformed (`CodecError`), like a `snap` without `seq` today.
- `record.get` and the ring fallback read the stamp with the value from the
  same slot, never separately. The reply is assembled around the encoded
  value (`session::aimx::value_reply`, shared with the bench) in an exactly
  sized buffer: one allocation more per `get` than the bare 3.x value. Event
  frames are unchanged. Removing that allocation would take the first
  `unsafe` block in core (filling an uninitialized `Arc<[u8]>`); not adopted
  for one allocation on an RPC.
- `record.set` echoes `value` and `ts` from a single readback after the
  write. The echo is the latest write observed, which under concurrent
  writers can be newer than this call's. When nothing is readable (a ring, or
  no latest value), the reply stays `{status}`.
- The WebSocket connector's `event` and `snap` frames carry `ts`, taken from
  `OutboundMessage::stamp()` (§4.4) and its snapshot cache.
- `record.query` (persistence) is unchanged: it returns stored history with
  the backend's persist time, not live values.

**`ts` in JavaScript.** Nanoseconds pass 2⁵³ after ~104 days of uptime,
beyond what `JSON.parse` represents exactly. Rust clients and the WASM
bindings (serde) are unaffected; browser code that parses frames itself
loses precision. `ts` stays an integer, and the protocol docs say so for JS
consumers. A string would cost every Rust client a parse.

## 6. Client and tools

- **`aimdb-client`:** `RecordUpdate` gains `ts: Stamp` (re-exported from
  `aimdb-core`). `get` returns `Stamped<serde_json::Value>`; `drain` returns
  a `DrainResponse` whose `values` are `Vec<Stamped<serde_json::Value>>`. An
  event or reply without `ts` is a protocol error (`ClientError::Protocol`). A `set` reply that carries
  `value`, `null` included, must carry `ts`; `{status}` alone is valid. The
  client has no clock accessor.
- **Wire decoding:** `Stamped<T>` and `WriteMetadata` in core have no serde
  derives. The client decodes replies through private wire types (`{value,
  ts}` and the drain envelope) and builds envelopes with
  `Stamped::new(value, WriteMetadata::from_wire_stamp(ts))`, so
  `DrainResponse` loses its serde derives. Client-side metadata holds what
  the wire carried and nothing more.
- **`aimdb watch`:** a `ts` column, labelled "time since instance start", in
  seconds with microsecond precision (`81.234567`). `--format json` prints
  one line per update, with `ts` in nanoseconds; stdout stays pure NDJSON
  and status messages go to stderr.
- **`aimdb record get`:** prints `ts` beside the value.
- **`aimdb-mcp`:** `get_record` and `drain_record` carry `ts` per value. No
  other tool changes.

## 7. What breaks

| Area | Change |
|---|---|
| `RuntimeOps` | `now_nanos` and `unix_time` removed; `new_clock` added, which out-of-tree adapters implement. |
| Clock and context | `InstanceClock` carries `now()` and `unix_time()`. `RuntimeContext` holds a clock; `ctx.time().now()` returns `Stamp`. `run_client` takes a `RuntimeContext` instead of `Arc<dyn RuntimeOps>`. |
| Buffers | Element type `Stamped<T>`; `buffer_raw` / `buffer_with_cfg` signatures; custom and mock buffers. |
| Envelope | New `Stamped<T>` (public `value`, private metadata) and opaque `WriteMetadata`. |
| Readers | `recv` / `try_recv` / `latest` return `Stamped<T>`; `aimdb-sync` likewise. `RecordValue` gains `metadata()` / `stamp()`. |
| JSON reads | `JsonBufferReader`, `JsonReader`, `JsonRecordAccess::latest_json*` and `AimDb::try_latest_as_json*` return `Stamped<serde_json::Value>` / `Stamped<Vec<u8>>` (§4.4). |
| Joins | `JoinTrigger::Input` gains `metadata` and becomes `#[non_exhaustive]`; `metadata()` / `stamp()` added. |
| Outbound | `OutboundMessage` gains `metadata`; `stamp()` added. |
| AimX | 4.0: mandatory `ts`, new `get` / `drain` / `set` reply shapes. 3.x peers are refused. |
| `aimdb-client` | `RecordUpdate.ts`, `get` / `drain` return types, `DrainResponse` without serde derives. |
| Callers | Examples, demos and docs that read a received value use `.value`. Generated code is unaffected: it reads no record values. |

Both phases (§8) ship in one PR, with CHANGELOG entries in the crates they
touch.

## 8. Phases

| Phase | Content | Crates |
|---|---|---|
| 1 | `Stamp`, `InstanceClock`, `Stamped<T>` / `WriteMetadata` and their conversions, the instance clock bound in `build()`, buffer element type, stamping in `RecordWriter`, reader / latest / join-trigger / outbound exposure | core, tokio / embassy / wasm adapters, sync, data-contracts, persistence, connectors' library code, examples, bench |
| 2 | AimX 4.0, client, CLI, MCP | core, client, cli, mcp, websocket / uds / serial / tcp connectors, wasm adapter, bench |

Phase 2 depends on phase 1. Both should reach users in the same release, so
4.0 servers and stamped in-process reads appear together.

## 9. Tests

| Crate | Cases |
|---|---|
| `aimdb-core` (envelope) | `map`, `try_map` and `as_ref` keep `metadata()` equal to the input's; `try_map` errors drop nothing else; `RecordValue`, `JoinTrigger` and `OutboundMessage` report the metadata of the write they carry; a transform output's metadata differs from its input's under a manual clock. |
| `aimdb-core` | every write path stamps (source, `try_produce`, transform, join output, inbound dispatch, `record.set`); `latest` and `recv` return the same stamp for the same write; with a manual clock, write stamps equal the manual time and a transform output carries its own write time, not its input's; one writer's stamps are nondecreasing; `db.now()`, `ctx.time().now()` and profiling read the same clock. |
| `aimdb-core` (JSON reads) | for one write, `latest_json`, `latest_json_bytes`, `try_latest_as_json*`, and each `JsonBufferReader` / `JsonReader` method (tree and bytes, poll and try) return the typed read's stamp; the trait-default byte paths keep it; a write landing between two reads never gives a payload the newer stamp (single-read check with a buffer whose `peek` is counted); encoded payloads equal the codec's output for `T` alone. |
| `aimdb-core` (AimX) | `ts` on `event`, `snap`, `get`, `drain` and `set`; `set` on a ring replies `{status}`; a value frame without `ts` is malformed; a 3.x client is refused. |
| `aimdb-tokio-adapter` | under `tokio::time::pause`, two writes with no `advance` share a stamp and `advance(d)` between them gives exactly `d`; readings from a `std::thread` and from a second runtime use the instance runtime's clock; an `aimdb-sync` `set` is stamped on the instance's paused clock. |
| `aimdb-embassy-adapter` | stamps are measured from `build()`; buffer contract tests (`test_support`) run with `Stamped<T>`; slot size of the default `CAP = 16` `f32` buffer asserted. |
| `aimdb-wasm-adapter` | write stamps are nondecreasing. |
| `aimdb-websocket-connector` | `ts` in `event` and `snap` frames; a `?v=3.x` URL is refused. |
| `aimdb-client` / CLI / MCP | `ts` surfaced per update, per `get` and per `drain` item; a reply without `ts` is a protocol error; a `set` reply with `value` (including `null`) but no `ts` is refused, `{status}` is accepted; `watch --format json` stays valid NDJSON through startup, gaps, snapshot completion and shutdown. |

## 10. Alternatives considered

1. **Optional stamp (`Option<Stamp>` or a sentinel).** Needed only to carry
   imported or missing times, which this design rules out. A mandatory stamp
   keeps every API and frame one shape.
2. **Inherited stamps through transforms, or imported stamps from links.**
   Turn the stamp into "time of some earlier observation, maybe on another
   clock", which needs per-value clock identity and writer restrictions. The
   data contract is the right place for origin times.
3. **Timestamp field in each contract** (`ContractInfo::timestamp_field`).
   Still the answer for measurement time (§1), but not for write time, which
   every record should have without each developer adding a field.
4. **A parallel `u64` ring beside each buffer.** Avoids padding for small
   `T`, but needs a second index kept in step with every adapter's buffer,
   including Tokio's lock-free broadcast. Rejected.
5. **`Stamped<T>` with public `value` and `stamp` fields.** Simplest, but
   invites callers to build and destructure the exact layout; the next
   metadata field would break them again. Rejected for private metadata
   behind accessors (§4.1).
6. **Generic metadata (`Stamped<T, M>`), a metadata map, or plugin hooks.**
   Add a type parameter to every record API, or allocations per write, for
   no current requirement. A concrete `WriteMetadata` grows by editing one
   type.
7. **Serialize `WriteMetadata` onto the wire as a whole.** Would let an
   internal field change protocol messages silently. The encoder picks
   fields explicitly (§5).
8. **Keep time reads on `RuntimeOps`, behind an instance wrapper.** An
   `InstanceRuntime` whose `now_nanos()` reads the instance clock would have
   kept connector code unchanged, but split ownership: monotonic reads through
   the clock, wall-clock reads through the runtime. Moving both onto the clock
   gives each time source one owner (§3.1).
9. **Keep 3.x compatibility.** Optional `ts`, reply negotiation and
   unstamped API variants double the surface for a transition no deployment
   needs; a major version is cheaper.

## 11. Resolved questions

1. **`ts` as a JSON integer.** Kept as integer nanoseconds and documented for
   JS consumers (§5.1).
2. **Restart detection.** No instance-generation identifier on the wire;
   stamps are scoped to one instance lifetime and a client resets its
   comparisons on reconnect (§1). An identifier is deferred to
   reconnect-aware caching or resumable subscriptions (§2 non-goals).
