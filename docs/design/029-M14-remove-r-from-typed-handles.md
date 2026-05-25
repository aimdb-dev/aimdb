# Remove `R` from `Producer<T>` and `Consumer<T>`

**Version:** 0.4 (implemented — produce/subscribe collapsed to sync)
**Status:** ✅ Implemented
**Issue:** TBD
**Depends on:** [M13 — Remove `Spawn` Trait](028-M13-remove-spawn-trait.md) (#88)
**Last Updated:** May 25, 2026
**Milestone:** M14 — Architectural clean-up

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
  - [The actual headline: hot-path lookup elimination](#the-actual-headline-hot-path-lookup-elimination)
  - [Cosmetic win: simpler user-facing signatures](#cosmetic-win-simpler-user-facing-signatures)
  - [What this is *not*](#what-this-is-not)
- [Current Architecture](#current-architecture)
  - [Why `R` is present today](#why-r-is-present-today)
  - [What does NOT need `R` at runtime](#what-does-not-need-r-at-runtime)
  - [The full `R`-propagation surface](#the-full-r-propagation-surface)
- [Proposed Design](#proposed-design)
  - [New `WriteHandle<T>` indirection](#new-writehandlet-indirection)
  - [`RecordWriter<T>` construction (Option B)](#recordwritert-construction-option-b)
  - [`Producer<T>` — after](#producert--after)
  - [`Consumer<T>` — after](#consumert--after)
  - [`ConnectorBuilder` — cascade (zero-LOC)](#connectorbuilder--cascade-zero-loc)
- [Type-System Changes](#type-system-changes)
- [Breaking Changes](#breaking-changes)
- [Implementation Plan](#implementation-plan)
- [Decisions](#decisions)
- [Out of Scope](#out-of-scope)

---

## Summary

Replace `Producer<T, R>`'s `Arc<AimDb<R>>` field (and the string-keyed lookup
it forces on every `produce()`) with a pre-resolved `Arc<dyn WriteHandle<T>>`
that points directly at the record's buffer + latest-snapshot + metadata.
Replace `Consumer<T, R>`'s same `Arc<AimDb<R>>` with a pre-resolved
`Arc<dyn DynBuffer<T>>`. The runtime parameter `R` disappears from both
public types.

The substantive change is **on the hot path**: today every `producer.produce(value).await`
does a `HashMap<StringKey, RecordId>` lookup, a bounds check, a `TypeId`
equality check, and a `dyn AnyRecord → &TypedRecord<T, R>` downcast before
the actual buffer push. After M14 it does one virtual call. The R-removal in
type signatures is the bonus, not the headline.

`ConnectorBuilder<R>` keeps its trait-level `R` parameter (Option A in
v0.1) — but the cascade is in practice zero-LOC because no connector struct
in the tree carries an `R` phantom today (M13 already cleaned them).

---

## Motivation

### The actual headline: hot-path lookup elimination

Every `producer.produce(value).await` call today follows this path:

```rust
// Producer::produce
self.db.produce::<T>(&self.record_key, value).await
  // → AimDb<R>::produce
  → self.inner.get_typed_record_by_key::<T, R>(key)?
       // HashMap<StringKey, RecordId>  lookup on key
       // bounds check on  id.index() < self.storages.len()
       // TypeId::of::<T>() == self.types[id.index()]  equality check
       // dyn AnyRecord → &TypedRecord<T, R>  downcast
  → typed_rec.produce(value).await
       // latest_snapshot.lock() = Some(value.clone())
       // buf.push(value)
       // metadata.mark_updated()
```

That is one HashMap probe, two equality checks, and a downcast on **every
single produce** — regardless of where the call originates, because the
producer carries the record key as a `String` and has to resolve it each
time. For a sensor producing at 1 kHz, or a transform on a busy stream,
those operations dominate the actual buffer push.

After M14 the producer holds a pre-resolved `Arc<dyn WriteHandle<T>>` that
points straight at the buffer + snapshot + metadata for one specific record.
The same call becomes:

```rust
// Producer::produce
self.write.push(value)   // ← one virtual call, then the real work
```

Same story on the consume side: today every `consumer.subscribe()` does the
same key-lookup before calling `buffer.subscribe_boxed()`. After M14 the
buffer Arc is pre-resolved at build time — `subscribe()` collapses to one
virtual call.

This is the substantive win and the reason to do M14. Pre-resolved at build,
not per-call: Producer/Consumer become "handles to a buffer", not "tickets to
look up a buffer".

### Cosmetic win: simpler user-facing signatures

After the hot-path change, `R` falls out of `Producer<T>` / `Consumer<T>`
naturally — the `Arc<AimDb<R>>` field is gone. That makes user-written
`.source()` / `.tap()` helper signatures shorter:

```rust
// before
async fn produce_temperature(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) { ... }

// after
async fn produce_temperature(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature>,
) { ... }
```

Codebase audit (M14-uncommitted): ~18 such occurrences across `examples/`
and `aimdb-pro/`. Shared helpers in `examples/{mqtt,knx}-connector-demo-common/`
become slightly less generic-soup too:

```rust
// before
pub async fn light_monitor<R>(ctx: RuntimeContext<R>, consumer: Consumer<LightState, R>)
// after
pub async fn light_monitor<R>(ctx: RuntimeContext<R>, consumer: Consumer<LightState>)
```

`R` remains on `RuntimeContext<R>` because it exposes `ctx.time()` / `ctx.log()`
which need runtime-specific types — see *Out of Scope*.

### What this is *not*

This refactor will not significantly reduce LOC. The trade is roughly
neutral: a new `WriteHandle` trait (~10 LOC) + a `RecordWriter<T>` struct
(~30 LOC) replace ~30 LOC of `R` mentions across the codebase. If the goal
is line-count reduction, M14 will not deliver — measure success by *hot-path
cycles eliminated* and *type-signature simplification*, not by `git diff
--stat`.

---

## Current Architecture

### Why `R` is present today

```
Producer<T, R> {
    db:         Arc<AimDb<R>>,   // ← the only source of R at runtime
    record_key: String,
    profiling:  Option<Arc<ProducerProfilingState>>,   // R-free
}

produce(value: T):
    db.produce::<T>(&record_key, value)   // key lookup → TypedRecord<T,R>
      → typed_record.produce(value)        // push to buffer + snapshot
```

`AimDb<R>::produce()` is the indirection: it resolves `record_key` to a
`TypedRecord<T, R>` using `get_typed_record_by_key::<T, R>()`. The `R` in the
return type is only needed for the downcast inside that lookup.

`Consumer<T, R>` follows the same pattern: `subscribe()` calls
`db.subscribe::<T>(&record_key)` which returns `Box<dyn BufferReader<T> + Send>`
— already type-erased at the return boundary.

### What does NOT need `R` at runtime

| Component | R needed? | Why |
|---|---|---|
| `ProducerProfilingState` | ✗ | `Clock = Arc<dyn Fn() -> u64 + Send + Sync>`, erased at build time |
| Buffer write (`DynBuffer::push`) | ✗ | Already `dyn`, no R in trait |
| Buffer subscribe (`DynBuffer::subscribe_boxed`) | ✗ | Returns `Box<dyn BufferReader<T>>` |
| `latest_snapshot` update | ✗ | `Arc<Mutex<Option<T>>>`, no R |
| Key → buffer resolution | ✓ | Currently lives inside `AimDb<R>`, fixable by moving it to build time |

### The full `R`-propagation surface

After M13 (uncommitted), these public items still carry `R`:

```
Producer<T, R>              — user closures in .source(), .tap()
Consumer<T, R>              — user closures in .tap(), connector subscribe
RecordRegistrar<R>          — registration API (build time only)
OutboundConnectorBuilder<T,R>
InboundConnectorBuilder<T,R>
ConnectorBuilder<R>         — public connector-author trait
  ↳ impl ConnectorBuilder<R> for MqttConnector<R>
  ↳ impl ConnectorBuilder<R> for KnxConnector<R>
  ↳ impl ConnectorBuilder<R> for WsConnector<R>
aimdb-codegen emitted types  — <R: RuntimeAdapter> on every generated registrar
```

---

## Proposed Design

### New `WriteHandle<T>` indirection

Add a crate-private trait (alongside `DynBuffer`) in
`aimdb-core/src/buffer/traits.rs`:

```rust
/// Write-side handle for a single record (issue #88 follow-up, design 029).
///
/// `Producer<T>` holds `Arc<dyn WriteHandle<T>>` so it can be parameterised
/// over `T` only — no runtime adapter `R` and no record-key string lookup on
/// the produce hot path. The implementor (`RecordWriter<T>`) pre-binds the
/// underlying buffer, the latest-snapshot slot, and the metadata tracker at
/// build time.
///
/// Crate-private on purpose: `Producer<T>::new` is the only construction
/// path. External callers that want to mock a Producer should go through
/// that constructor (or a future `Producer::for_testing(...)` helper) —
/// promoting the trait to `pub` is reserved for an explicit testing-DX
/// follow-up if/when there's demand.
pub(crate) trait WriteHandle<T: Clone + Send + 'static>: Send + Sync {
    /// Push a value into the buffer, update the latest-snapshot cache, and
    /// (when a buffer is present) mark the metadata `last_update` timestamp.
    /// Infallible — all three operations are synchronous and lock-free or
    /// spin-locked.
    fn push(&self, value: T);
}
```

The visibility is deliberately `pub(crate)`. `Producer<T>::new` is the only
construction path; external test code that needs a fake Producer goes through
a `for_testing(buffer: Arc<dyn DynBuffer<T>>)` constructor we can add later
if demand materialises.

### `RecordWriter<T>` construction (Option B)

The implementor of `WriteHandle<T>` is `RecordWriter<T>`, a plain struct
holding *clones of* the three shared Arcs the record already owns:

```rust
// aimdb-core/src/buffer/writer.rs  (new file)

pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    /// `None` for records that only support `latest()` (no buffer configured).
    buffer: Option<Arc<dyn DynBuffer<T>>>,

    /// Snapshot slot shared with `TypedRecord` and any `latest()` reader.
    #[cfg(feature = "std")]
    latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,
    #[cfg(not(feature = "std"))]
    latest_snapshot: Arc<spin::Mutex<Option<T>>>,

    /// Metadata tracker (already `Clone` with shared inner `Arc<Mutex>` /
    /// `Arc<AtomicBool>`). std-only.
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,
}

impl<T: Clone + Send + 'static> WriteHandle<T> for RecordWriter<T> {
    fn push(&self, value: T) {
        #[cfg(feature = "std")]
        { *self.latest_snapshot.lock().unwrap() = Some(value.clone()); }
        #[cfg(not(feature = "std"))]
        { *self.latest_snapshot.lock() = Some(value.clone()); }

        if let Some(buf) = &self.buffer {
            buf.push(value);
            #[cfg(feature = "std")]
            self.metadata.mark_updated();
        }
    }
}
```

**Construction site:** `RecordFutureCollector::collect_all_futures` /
`TypedRecord::collect_producer_future`. The `RecordWriter` is built just
before the Producer is handed to the user closure — once per producer per
record. Producer clones share the same `Arc<RecordWriter<T>>`.

**Ownership choice (vs. Option A).** Option A was "switch
`AimDbInner.storages: Vec<Box<dyn AnyRecord>>` to `Vec<Arc<dyn AnyRecord>>`
and upcast `Arc<TypedRecord<T, R>>` to `Arc<dyn WriteHandle<T>>`". Rejected:
that changes the ownership model of every record everywhere and touches
every consumer of `AimDbInner::storage() -> &dyn AnyRecord`. Option B
isolates the change to `TypedRecord`'s internals — from the outside,
`AimDbInner` still hands out `&dyn AnyRecord`.

**Single structural change to `TypedRecord<T, R>`.** Its
`buffer: Option<Box<dyn DynBuffer<T>>>` becomes
`Option<Arc<dyn DynBuffer<T>>>`. Everything else (`latest_snapshot`,
`metadata`) is already shareable (`Arc<Mutex<...>>`, `Clone`). `TypedRecord`
gains two accessors used at build time only:

```rust
fn writer_handle(&self) -> Arc<dyn WriteHandle<T>>;   // builds a fresh RecordWriter
fn buffer_handle(&self) -> Option<Arc<dyn DynBuffer<T>>>;
```

`TypedRecord::produce()` (still callable via `AimDb::produce(key, value)`)
continues to work for the few legacy paths that have a key but not a
pre-built Producer — it internally constructs / reuses a writer.

### `Producer<T>` — after

```rust
pub struct Producer<T> {
    write: Arc<dyn WriteHandle<T>>,
    #[cfg(feature = "profiling")]
    profiling: Option<Arc<ProducerProfilingState>>,
    _phantom: PhantomData<fn() -> T>,  // keeps Send/Sync independent of T
}
```

Gone: the `db: Arc<AimDb<R>>` field, the `record_key: String` field, the
`R` type parameter, the `Producer::key() -> &str` accessor.

`produce()` becomes:

```rust
pub fn produce(&self, value: T) {
    #[cfg(feature = "profiling")]
    if let Some(state) = &self.profiling {
        state.record_produce();
    }
    self.write.push(value);
}
```

**Signature change (v0.4 revision).** The earlier draft preserved
`async fn ... -> DbResult<()>` for source-level compatibility. After
implementation we reversed that decision: every reachable code path is
synchronous and infallible (one virtual `WriteHandle::push`), so the wrapper
was pure ceremony. Keeping `async`/`Result` would have meant `.await?` at
every call site forever — propagating fictional fallibility to please a
type signature.

**Trade-off accepted.** `producer.produce(x).await?` no longer compiles —
call sites drop both the `.await` and the `?`. This is a one-time edit on
~50 examples / aimdb-pro sites and is called out as a breaking change in
the aimdb-core 1.1.0 CHANGELOG. If backpressure-aware buffers or
shutdown-signalling semantics show up in a future milestone, they can be
introduced as a new `produce_async(&self, value: T) -> impl Future<Output =
DbResult<()>>` method alongside the sync one — additive, not breaking.

The type-erased trait surface keeps a `Future<Output = Result<...>>`:
`ProducerTrait::produce_any` still returns a pinned future because its
fallibility is real (the `Any` downcast can fail when a route delivers the
wrong type).

### `Consumer<T>` — after

```rust
pub struct Consumer<T> {
    buffer: Arc<dyn DynBuffer<T>>,
    #[cfg(feature = "profiling")]
    profiling: Option<(Arc<StageMetrics>, Clock)>,
    _phantom: PhantomData<fn() -> T>,
}
```

`subscribe()` collapses to:

```rust
pub fn subscribe(&self) -> Box<dyn BufferReader<T> + Send> {
    let reader = self.buffer.subscribe_boxed();
    #[cfg(feature = "profiling")]
    if let Some((metrics, clock)) = &self.profiling {
        return Box::new(ProfilingBufferReader::new(reader, metrics.clone(), clock.clone()));
    }
    reader
}
```

`DynBuffer<T>` already exists and is the natural read-side counterpart — no
new trait required.

Same v0.4 revision as `produce()`: the buffer Arc is pre-resolved at
`Consumer::new`, so the lookup that previously produced `DbResult` is gone
and there is nothing left to fail. The `DbResult<>` wrapper was dropped to
avoid forcing a `.unwrap()` / `?` at every call site. The type-erased
`ConsumerTrait::subscribe_any` keeps its `DbResult` (factories still resolve
records through a downcast that can fail) — see `impl ConsumerTrait for
Consumer<T>` in `typed_api.rs`, which wraps the infallible inner call in
`Ok(...)`.

### `ConnectorBuilder` — cascade (zero-LOC)

`ConnectorBuilder<R>` keeps its trait-level `R` parameter (v0.1's Option A).
But the cascade in connector *struct* code is in practice **zero LOC**: a
grep of the workspace shows no connector struct carries an `R` phantom
today:

```text
MqttConnectorBuilder, KnxConnectorBuilder, WebSocketConnectorBuilder,
MqttConnectorImpl,    KnxConnectorImpl,    WebSocketConnectorImpl
```

All six are non-generic. M13 already cleaned the field layer; M14 inherits
that. Connector authors do not see Producer/Consumer as typed values either
— routes are returned via `Box<dyn ConsumerTrait>` / `Box<dyn ProducerTrait>`
(fully erased) from `db.collect_inbound_routes()` /
`db.collect_outbound_routes()`. So the only `R` mention left in connector
crates after M14 is on the `impl<R: aimdb_executor::RuntimeAdapter + 'static>
ConnectorBuilder<R> for X` line itself — unchanged.

Full `ConnectorBuilder` non-generic-isation (v0.1's Option B) is still
deferred to a separate milestone (needs a `ConnectorContext` abstraction).

---

## Type-System Changes

| Before | After |
|---|---|
| `Producer<T, R>` | `Producer<T>` |
| `Consumer<T, R>` | `Consumer<T>` |
| `Producer::key(&self) -> &str` | **removed** |
| `Producer::produce(&self, T) -> impl Future<Output = DbResult<()>>` | `pub fn produce(&self, T)` — synchronous, infallible (v0.4 revision) |
| `Consumer::subscribe(&self) -> DbResult<…>` | `pub fn subscribe(&self) -> Box<dyn BufferReader<T> + Send>` — infallible (v0.4 revision) |
| `TypedRecord::buffer: Option<Box<dyn DynBuffer<T>>>` | `Option<Arc<dyn DynBuffer<T>>>` |
| `RecordRegistrar<R>` (build-time only) | unchanged — still needs `R` to resolve records |
| `ConnectorBuilder<R>` | unchanged trait |
| `impl<R: RuntimeAdapter + 'static> ConnectorBuilder<R> for MqttConnectorBuilder` | unchanged (already R-free in the struct) |
| Codegen emitted `<R: RuntimeAdapter + 'static>` on registrars | unchanged — registrars are build-time |
| `RuntimeContext<R>` | unchanged — see *Out of Scope* |

`RecordRegistrar<R>` is intentionally kept generic: it is a build-time-only
object (not stored past `build()`) and needs `R` to call
`AimDb<R>::register_record_typed()`. It does not appear in user code after
setup (inference handles `R` in `|reg| { ... }` closures).

---

## Breaking Changes

**Public API (semver minor bump):**

- `Producer<T, R>` → `Producer<T>` — any code that names the two-parameter form breaks. Code that uses `Producer` via type inference (the common case) compiles without changes.
- `Consumer<T, R>` → `Consumer<T>` — same.
- `Producer::key()` is **removed**. Code that called `producer.key()` for diagnostic strings should capture the key at the registration site instead. Two internal callers in `aimdb-core` (`run_single_transform`, `run_join_transform`) are updated as part of step 7.
- **`Producer::produce()` is now `pub fn produce(&self, value: T)` — synchronous and infallible** (v0.4 revision). Every `producer.produce(x).await?` call site must drop both the `.await` and the `?`. ProducerTrait::produce_any keeps its async/Result surface for type-erased routing where the Any downcast can still fail.
- **`Consumer::subscribe()` is now `pub fn subscribe(&self) -> Box<dyn BufferReader<T> + Send>`** — the `DbResult<>` wrapper is gone because the buffer Arc is pre-resolved at `Consumer::new`. Call sites drop the `?`. ConsumerTrait::subscribe_any keeps its `DbResult` for factory-side fallibility.

**Codegen:** emitted code does not reference `Producer` / `Consumer` with type parameters directly. Golden-test strings update mechanically; no template changes.

**Connector authors:** zero-LOC impact. Connector structs are already R-free (M13), and routes are already type-erased via `ConsumerTrait` / `ProducerTrait`. The `impl<R: RuntimeAdapter + 'static> ConnectorBuilder<R> for …` line stays as written.

**`aimdb-pro` demo binaries:** ~6 weather-station files plus `weather-hub-streaming` have factored-out `async fn` producers/consumers that name `Producer<T, TokioAdapter>` / `Consumer<T, TokioAdapter>`. Drop the `, TokioAdapter` from each. No structural change.

---

## Implementation Plan

Listed in dependency order. Each step should pass `make check` before the
next begins (modulo the call-site updates in step 10, which are gated by
steps 1–9 landing first).

### Step 1 — Add the write-side primitives (no external break)

**Files:** `aimdb-core/src/buffer/traits.rs`, `aimdb-core/src/buffer/writer.rs` (new), `aimdb-core/src/buffer/mod.rs`

- Add `pub(crate) trait WriteHandle<T>` to `traits.rs`.
- Add `pub(crate) struct RecordWriter<T>` + `impl WriteHandle<T> for RecordWriter<T>` to a new `writer.rs`.
- Re-export from `mod.rs` (crate-private).

### Step 2 — `TypedRecord` Box → Arc + accessors

**File:** `aimdb-core/src/typed_record.rs`

- Change `buffer: Option<Box<dyn DynBuffer<T>>>` → `Option<Arc<dyn DynBuffer<T>>>`.
- Add `pub(crate) fn writer_handle(&self) -> Arc<dyn WriteHandle<T>>` — constructs a `RecordWriter` with cloned Arcs.
- Add `pub(crate) fn buffer_handle(&self) -> Option<Arc<dyn DynBuffer<T>>>` — clones the buffer Arc.
- Leave `TypedRecord::produce()` callable for the legacy key-based path (`AimDb::produce(key, value)`).

### Step 3 — `Producer<T>` removes `R`

**File:** `aimdb-core/src/typed_api.rs`

- Drop type parameter `R` from `Producer<T, R>` → `Producer<T>`.
- Replace `db: Arc<AimDb<R>>` with `write: Arc<dyn WriteHandle<T>>`.
- Delete `record_key: String` field and the `pub fn key(&self) -> &str` accessor (two internal callers `run_single_transform` / `run_join_transform` are updated in step 7 to capture the key via closure instead).
- Change `produce()` signature to `pub fn produce(&self, value: T)` (v0.4 revision — synchronous and infallible; see Decision 2).
- Update `impl ProducerTrait for Producer<T>` likewise — keeps async/Result for type-erased fallibility.

### Step 4 — `Consumer<T>` removes `R`

**File:** `aimdb-core/src/typed_api.rs`

- Drop type parameter `R` from `Consumer<T, R>` → `Consumer<T>`.
- Replace `db: Arc<AimDb<R>>` field with `buffer: Arc<dyn DynBuffer<T>>`.
- Update `subscribe()` to call `self.buffer.subscribe_boxed()` directly (no error path through `AimDb::subscribe`). Return `Box<dyn BufferReader<T> + Send>` directly — drop the `DbResult<>` wrapper (v0.4 revision).
- Update `impl ConsumerTrait for Consumer<T>` likewise — keeps `DbResult` on the trait surface for factory-side fallibility.

### Step 5 — `collect_*` construction sites

**File:** `aimdb-core/src/typed_record.rs`

- `collect_producer_future`: build `RecordWriter` from the typed record, construct `Producer::new(writer_handle)`. The producer no longer needs the `record_key` argument.
- `collect_consumer_futures`: clone `buffer_handle()`, construct `Consumer::new(buffer)`.
- `collect_transform_futures`: same as producer (transform produces into its own record).

### Step 6 — `RecordRegistrar<R>` internal Producer/Consumer construction

**File:** `aimdb-core/src/typed_api.rs`

- Update the (build-time-only) call sites inside `RecordRegistrar` that construct `Producer` / `Consumer` (e.g. for `.tap_raw()` factory closures, for the inbound-connector factory) to use the new constructors.
- The registrar keeps its `R` parameter — it still needs `R` to call `AimDb<R>::register_record_typed()`. `R` does not leak past `build()`.

### Step 7 — Transforms (`run_single_transform`, `run_join_transform`)

**Files:** `aimdb-core/src/transform/single.rs`, `aimdb-core/src/transform/join.rs`

- Update typed signatures: `Producer<O, R>` → `Producer<O>`, `Consumer<I, R>` → `Consumer<I>`.
- Where `producer.key()` was used for tracing strings, capture the key into the spawning closure instead and pass it as a parameter.

### Step 8 — Connector cascade (zero structural changes)

**Files:** `aimdb-mqtt-connector`, `aimdb-knx-connector`, `aimdb-websocket-connector`

- No struct changes (verified: no connector struct carries `R`).
- `impl<R: aimdb_executor::RuntimeAdapter + 'static> ConnectorBuilder<R> for X` lines stay as-is.
- Routes are already type-erased via `ConsumerTrait` / `ProducerTrait`.

### Step 9 — Codegen golden tests

**File:** `aimdb-codegen/src/rust.rs`

- Emitted `configure_schema<R: RuntimeAdapter + 'static>(...)` is unchanged (registrar still has `R`).
- Producer/Consumer are not referenced in emitted code; no template changes.
- Golden-test strings: verify Producer/Consumer mentions (if any) are updated.

### Step 10 — Examples + aimdb-pro + tests

Drop `R` from Producer/Consumer in ~18 user-side signatures across:

- `examples/{mqtt,knx,embassy-{mqtt,knx},tokio-{mqtt,knx},remote-access,hello-single-latest-async,weather-mesh-demo}-*`
- `aimdb-pro/demo/weather-station-*`
- Generic helpers in `examples/{mqtt,knx}-connector-demo-common/src/monitors.rs` (the `<R>` parameter on the fn stays for `RuntimeContext<R>`, the Producer/Consumer params drop their `R`).

### Step 11 — CHANGELOG + README

- Each affected crate's CHANGELOG gains an `[Unreleased]` entry.
- Lead with the **hot-path lookup elimination** narrative (per the Motivation section above), with R-removal as the secondary benefit.
- Workspace README quickstart already calls `builder.run().await?` (post-M13); add a `.source()` example to demonstrate `Producer<T>` (no R).

---

## Decisions

All open questions raised in design review (May 24, 2026) are resolved:

1. **`Producer::key()` accessor** → **Delete**, along with the `record_key: String` field. Two internal callers (`run_single_transform`, `run_join_transform`) capture the key into the spawning closure instead. External users get the key from the registration site by construction, not from the Producer.

2. **`Producer::produce()` / `Consumer::subscribe()` return types** → **Collapse to sync, infallible** (v0.4 revision — reversing the v0.3 decision). The body has no async or fallible operations after M14: `produce` is one virtual `WriteHandle::push`, `subscribe` is one virtual `DynBuffer::subscribe_boxed`. Keeping `async fn ... -> DbResult<()>` would have forced `.await?` at every call site forever, propagating fictional fallibility purely to please a signature. The one-time `.await?` removal across ~50 examples / aimdb-pro sites is worth the cleaner steady state. Future backpressure-aware variants can be added as additional methods (`produce_async`, `try_produce`) — additive, not breaking. The type-erased trait surfaces (`ProducerTrait::produce_any`, `ConsumerTrait::subscribe_any`) keep async/Result for genuine fallibility (downcast, factory resolution).

3. **`ConnectorBuilder<R>` cascade** → **v0.1 Option A**, but with the realisation that it is **zero LOC** for connector structs. Verified: no connector struct in `aimdb-mqtt-connector`, `aimdb-knx-connector`, or `aimdb-websocket-connector` carries an `R` phantom today (M13 already cleaned them). The only `R` left is on the `impl<R> ConnectorBuilder<R> for X` line and stays untouched. Full non-generic-isation is deferred to a separate milestone.

4. **`RuntimeContext<R>` removal** → **Out of scope for M14.** `RuntimeContext<R>` exposes `ctx.time()` / `ctx.log()` which depend on `R::TimeOps` / `R::Logger` associated types. Erasing those behind `dyn` is a larger change that warrants its own milestone. Users keep writing `ctx: RuntimeContext<R>` next to `producer: Producer<T>`.

5. **`Arc<dyn WriteHandle<T>>` construction** → **Option B.** A `RecordWriter<T>` struct lives in `aimdb-core/src/buffer/writer.rs`, holding cloned Arcs of the record's buffer, latest-snapshot, and metadata. Constructed at producer-creation time. `AimDbInner.storages: Vec<Box<dyn AnyRecord>>` is **unchanged** — only the Box→Arc on `TypedRecord::buffer` propagates. Rejected Option A (switching storages to `Arc<dyn AnyRecord>`) was broader than necessary.

6. **`WriteHandle<T>` visibility** → **`pub(crate)`.** External mocking goes through `Producer::new` (or a future `Producer::for_testing` helper) — no need to expose the trait. Promote to `pub` only if a concrete testing-DX request materialises.

---

## Out of Scope

- **`RuntimeContext<R>` removal.** Same pattern as Producer/Consumer in spirit (R is the only generic), but `ctx.time()` / `ctx.log()` return runtime-specific associated types (`R::Instant`, `R::Duration`, etc.). Erasing those behind `dyn` is a structurally larger change that touches every user `ctx.time().sleep(d).await` call site. Worth its own milestone with a dedicated trait-object design (e.g. `DurationProvider`, `LoggerProvider`). Until then, users keep writing `ctx: RuntimeContext<R>` next to `producer: Producer<T>` — visually inconsistent but tolerable.
- **Full `ConnectorBuilder<R>` non-generic-isation** (v0.1's Option B). Requires a `ConnectorContext` abstraction exposing type-erased record access (e.g. `fn producer<T>(&self, key: &str) -> Producer<T>`). Larger API design in its own right. The trait-level `<R>` stays for M14.
- **`RecordRegistrar<R>` removal.** Registrar is build-time only; its `R` bound causes no propagation into user runtime code (inference handles it in the `|reg| { ... }` closure form, which is the only form used in practice — verified across `examples/` and `aimdb-pro/`). Clean-up possible but low value.
- **AimX remote handler generics.** The remote handler still has `R` bounds from deferred bridge-state work (see M13 §"Out of Scope"). Orthogonal to this change; addressed by the AimX portability follow-up.
- **Promoting `WriteHandle<T>` to `pub`.** Reserved for an explicit testing-DX follow-up if external users request mockable Producers. M14 keeps the trait `pub(crate)`.
