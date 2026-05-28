# Remove `latest_snapshot` — Buffer-Native Reads and Real Reader Slots

**Version:** 0.2 (proposal)  
**Status:** 🔵 Proposed  
**Depends on:** [M14 — Remove `R` from typed handles](029-M14-remove-r-from-typed-handles.md)  
**Last Updated:** May 27, 2026  
**Milestone:** M15 — Snapshot elimination

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
  - [The snapshot is redundant for every buffer type](#the-snapshot-is-redundant-for-every-buffer-type)
  - [The snapshot is updated twice via divergent paths](#the-snapshot-is-updated-twice-via-divergent-paths)
  - [std/no\_std cfg duplication](#stdno_std-cfg-duplication)
  - [The snapshot limits remote access to latest-only semantics](#the-snapshot-limits-remote-access-to-latest-only-semantics)
- [Current Architecture](#current-architecture)
  - [How `latest_snapshot` is used today](#how-latest_snapshot-is-used-today)
  - [Why it was added](#why-it-was-added)
  - [The two divergent write paths](#the-two-divergent-write-paths)
- [Proposed Design](#proposed-design)
  - [1. `DynBuffer::peek()` — buffer-native point-in-time read](#1-dynbufferpeek--buffer-native-point-in-time-read)
  - [2. AimX `record.get` uses `peek()` directly](#2-aimx-recordget-uses-peek-directly)
  - [3. AimX `record.drain` and `record.subscribe` use real `BufferReader`](#3-aimx-recorddrain-and-recordsubscribe-use-real-bufferreader)
  - [4. Remove `latest_snapshot` from `TypedRecord`](#4-remove-latest_snapshot-from-typedrecord)
  - [5. Remove the `latest_snapshot` Arc from `RecordWriter`](#5-remove-the-latest_snapshot-arc-from-recordwriter)
  - [6. Merge the two divergent update paths](#6-merge-the-two-divergent-update-paths)
- [Per-Buffer-Type Analysis](#per-buffer-type-analysis)
  - [SingleLatest (watch)](#singlelatestwwatch)
  - [Mailbox (notify + mutex slot)](#mailbox-notify--mutex-slot)
  - [SPMC Ring (broadcast)](#spmc-ring-broadcast)
  - [Bufferless records](#bufferless-records)
- [no\_std Impact](#nostd-impact)
- [Implementation Plan](#implementation-plan)
- [Breaking Changes](#breaking-changes)
- [Out of Scope](#out-of-scope)

---

## Summary

`TypedRecord<T, R>` carries a `latest_snapshot: Arc<Mutex<Option<T>>>` that
stores a redundant copy of every produced value. For `SingleLatest` records
the value already lives in the `watch::Sender`'s slot; for `Mailbox` records
it already lives in the `Mutex<Option<T>>` slot inside the buffer. The
snapshot is a second copy that adds a lock acquire on every `produce()` and
serves only the AimX `record.get` path.

This design replaces it with two things:

1. **`DynBuffer::peek()`** — a new optional method on the buffer trait that
   reads the buffer's own native storage non-destructively. For `SingleLatest`
   this is `watch_tx.borrow().clone()`; for `Mailbox` it is `slot.lock().clone()`;
   for SPMC Ring it returns `None` (no canonical latest in a history buffer).

2. **Real `BufferReader` slots for AimX** — `record.drain` and
   `record.subscribe` already use a per-connection `Box<dyn BufferReader<T>>`.
   `record.get` is changed to use `peek()` on the buffer instead of reading
   the snapshot. The reader IS the buffer reader — not a shadow copy.

The `latest_snapshot` field is removed entirely. For no\_std the benefit is
larger: the `spin::Mutex<Option<T>>` field and all its lock sites on the hot
`produce()` path disappear completely, since AimX does not exist in
embedded targets.

---

## Motivation

### The snapshot is redundant for every buffer type

`TokioBufferInner` and `EmbassyBufferInner` already store the value natively:

| Buffer type | Native storage | Snapshot storage |
|---|---|---|
| `SingleLatest` (Tokio) | `watch::Sender<Option<T>>` slot | `Arc<std::sync::Mutex<Option<T>>>` |
| `SingleLatest` (Embassy) | `Watch<CriticalSectionRawMutex, T, N>` slot | `Arc<spin::Mutex<Option<T>>>` |
| `Mailbox` (Tokio) | `Arc<StdMutex<Option<T>>>` + Notify | `Arc<std::sync::Mutex<Option<T>>>` |
| `Mailbox` (Embassy) | `Channel<_, T, 1>` | `Arc<spin::Mutex<Option<T>>>` |
| `SPMC Ring` | ordered ring (no canonical latest) | `Arc<Mutex<Option<T>>>` |

For `SingleLatest` and `Mailbox`, the value is stored twice. Every `produce()`
pays for two writes. For `SingleLatest` this is the most visible win:
`watch::Sender::send()` is lock-free, so removing the snapshot mutex makes
the produce path truly lock-free — the only mutex on `SingleLatest` produce
today is the snapshot one. For `Mailbox` it's a smaller change: two
uncontended `StdMutex` acquisitions collapse to one. For SPMC Ring the
snapshot is a third store alongside the ring slot and the broadcast queue.

### The snapshot is updated twice via divergent paths

Two independent code paths update the snapshot, with duplicated `#[cfg]` guards:

```rust
// Path 1 — TypedRecord::produce() (called by WASM adapter, builder init)
#[cfg(feature = "std")]
*self.latest_snapshot.lock().unwrap() = Some(val.clone());
#[cfg(not(feature = "std"))]
*self.latest_snapshot.lock() = Some(val.clone());

// Path 2 — RecordWriter::push() (called by Producer<T> via WriteHandle)
#[cfg(feature = "std")]
*self.latest_snapshot.lock().unwrap() = Some(value.clone());
#[cfg(not(feature = "std"))]
*self.latest_snapshot.lock() = Some(value.clone());
```

Both also duplicate the buffer push. There is no invariant preventing the two
paths from diverging (e.g., one being updated when the other is not).

### std/no\_std cfg duplication

The snapshot field and all its call sites repeat the
`#[cfg(feature = "std")] / #[cfg(not(feature = "std"))]` pair at approximately
ten locations in `typed_record.rs` and `buffer/writer.rs` alone (~60–80
cfg-conditional lines for a single conceptual thing). Removing the field
collapses all of them.

### The snapshot limits remote access to latest-only semantics

`record.get` today reads the single snapshot value — it can only ever return
the last value produced. For `SPMC Ring` records this is architecturally wrong:
the ring's purpose is to preserve a bounded history of N values for independent
consumers. A caller using AimX `record.get` cannot observe the buffer's window;
they always see only the most recent item. A real `BufferReader` for the ring
gives access to the full current window.

---

## Current Architecture

### How `latest_snapshot` is used today

```
produce(T)
  ├─ TypedRecord::produce()          (WASM adapter, builder)
  │    ├─ snapshot.lock() = Some(T)  ← snapshot write
  │    └─ buffer.push(T)
  └─ RecordWriter::push()            (Producer<T> via WriteHandle)
       ├─ snapshot.lock() = Some(T)  ← snapshot write (duplicate)
       └─ buffer.push(T)

record.get (AimX)
  └─ db.try_latest_as_json()
       └─ AnyRecord::latest_json()
            └─ snapshot.lock().clone()  ← snapshot read

record.drain (AimX)
  └─ conn_state.drain_readers[name].try_recv_json()  ← real BufferReader ✓

record.subscribe (AimX)
  └─ AnyRecord::subscribe_json()
       └─ buffer.subscribe_boxed()  ← real BufferReader ✓
```

`record.drain` and `record.subscribe` already use real `BufferReader`s.
Only `record.get` reads the snapshot.

### Why it was added

The snapshot was introduced for `record.get` to provide a point-in-time read
without subscribing a persistent reader. Subscribing a `watch::Receiver` at
request time gives `has_changed() = false` (the receiver starts at the current
version), so `try_recv()` returns `BufferEmpty` even when a value exists.
`borrow()` on the watch sender bypasses this — which is exactly what `peek()`
exposes.

### The two divergent update paths

`TypedRecord::produce()` exists alongside `RecordWriter::push()`. Both update
the snapshot and the buffer. Call sites of `TypedRecord::produce()`:

- `aimdb-wasm-adapter/src/bindings.rs:553` — WASM inbound JS→record handler
- `aimdb-wasm-adapter/src/ws_bridge.rs:760` — WebSocket bridge `produce_from_json`
- `aimdb-core/src/builder.rs:1106` — body of the public `AimDb::produce<T>(key, value)`
  method (per-call key lookup convenience wrapper)
- `aimdb-core/src/typed_record.rs:1716` — inside `AnyRecord::set_from_json()`,
  the AimX `record.set` path
- (transitive) `aimdb-core/src/database.rs:84` and `aimdb-sync/src/handle.rs:407`
  call `AimDb::produce` and are covered once the wrapper is migrated

All of these bypass `RecordWriter` and therefore bypass `RecordMetadataTracker`
(in std), meaning metadata (last-updated timestamp) is not updated on these
paths. After unification, every produce path will mark metadata — see
[Breaking Changes](#breaking-changes).

---

## Proposed Design

### 1. `DynBuffer::peek()` — buffer-native point-in-time read

Add an optional method to the `DynBuffer<T>` trait in `aimdb-core`:

```rust
pub trait DynBuffer<T: Clone + Send + 'static>: Send + Sync {
    fn push(&self, value: T);
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send>;
    fn as_any(&self) -> &dyn core::any::Any;

    /// Non-destructive read of the buffer's current value.
    ///
    /// Returns `Some(T)` if the buffer holds a current value that can be read
    /// without consuming it from any consumer's perspective. Returns `None`
    /// if the buffer type has no such concept (e.g., SPMC Ring) or if no
    /// value has been produced yet.
    ///
    /// This method does NOT advance any reader position. It is equivalent to
    /// a non-blocking borrow of the buffer's internal state.
    fn peek(&self) -> Option<T> {
        None
    }
}
```

**Tokio adapter implementations:**

```rust
// TokioBuffer::peek()
match &*self.inner {
    TokioBufferInner::Watch { tx } => {
        tx.borrow().clone()  // reads from sender's slot, no lock, no copy unless caller clones
    }
    TokioBufferInner::Notify { slot, .. } => {
        slot.lock().unwrap().clone()  // same Mutex already held by the Mailbox buffer
    }
    TokioBufferInner::Broadcast { .. } => None,
}
```

**Embassy adapter implementations** (when embedded introspection is needed):

```rust
// EmbassyBuffer::peek()
match &*self.inner {
    EmbassyBufferInner::Watch(watch) => {
        // Watch::try_get(&self) reads the current value without claiming one
        // of the bounded WATCH_N receiver slots. Do NOT use watch.receiver(),
        // which allocates a receiver and can fail when N is exhausted.
        watch.try_get()
    }
    EmbassyBufferInner::Mailbox(channel) => {
        // Channel<_, T, 1> has no non-destructive peek; None is acceptable
        None
    }
    EmbassyBufferInner::SpmcRing(_) => None,
}
```

The Embassy implementation of `peek()` is optional — the default `None` is
correct for no\_std targets where AimX does not exist.

### 2. AimX `record.get` uses `peek()` directly

Replace `db.try_latest_as_json()` → `snapshot.lock().clone()` with:

```rust
// handle_record_get:
let value = record.buffer().peek();  // Option<T> directly from buffer native storage
match value {
    Some(v) => serialize_to_json(&v),
    None => Response::error(request_id, "not_found", "No value available"),
}
```

`AnyRecord::latest_json()` is rewritten to call `buf.peek()` and serialise
directly, bypassing `RecordValue<T>` for this path. `latest()` and
`RecordValue<T>` remain in place for the typed in-process API and are dealt
with separately — see [Out of Scope](#out-of-scope). After Step 3 the read
path is `record.get` → `latest_json` → `buffer.peek()` → buffer-native storage.

**SPMC Ring (Broadcast) records — explicit semantic choice.** The Tokio
broadcast channel does not expose a non-destructive peek. There are two
options, and this design picks (B):

(A) **Maintain a "latest" alongside the ring.** Restores today's snapshot
behaviour for the ring but partially defeats the proposal — it reintroduces
a second store on the produce hot path, only for one buffer type.

(B) **`peek()` returns `None`, `record.get` returns `not_found`.** Clients
that need "give me the most recent value" on a ring use `record.drain` (which
consumes from their own per-connection cursor). Clients that need
point-in-time reads use `SingleLatest`, which is the buffer designed for it.
This is a behaviour change for ring-buffered records that previously called
`record.get`; callers must move to `record.drain` or reconfigure the record.

The earlier draft proposed a fallback "drain the per-connection reader from
inside `record.get`" — that is rejected because it consumes from the cursor
shared with `record.drain`, so interleaving `get` and `drain` on the same
connection would silently drop values from the drain stream.

### 3. AimX `record.drain` and `record.subscribe` use real `BufferReader`

This is already implemented. The per-connection `drain_readers` map in
`ConnectionState` holds a `Box<dyn JsonBufferReader>` per record, which is a
`BufferReader<T>` wrapped with a JSON serialization adapter. No changes needed
here.

The AimX reader IS a buffer reader — not a shadow copy. It has its own
independent position in the SPMC Ring, watches its own version counter for
`SingleLatest`, and drains the `Mailbox` slot destructively. This is identical
to how any `Consumer<T>` operates.

### 4. Remove `latest_snapshot` from `TypedRecord`

```rust
// Before
pub struct TypedRecord<T: Clone + Send + 'static, R> {
    // ...
    #[cfg(feature = "std")]
    latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,
    #[cfg(not(feature = "std"))]
    latest_snapshot: Arc<spin::Mutex<Option<T>>>,
    // ...
}

// After
pub struct TypedRecord<T: Clone + Send + 'static, R> {
    // latest_snapshot removed entirely
    // ...
}
```

The `latest()` method on `TypedRecord` / `AnyRecord` is replaced:

```rust
// Before — reads snapshot
fn latest_json(&self) -> Option<serde_json::Value> {
    let value = self.latest_snapshot.lock().unwrap().clone()?;
    // serialize...
}

// After — delegates to buffer peek
fn latest_json(&self) -> Option<serde_json::Value> {
    let buf = self.buffer.as_ref()?;
    let value = buf.peek()?;
    // serialize...
}
```

### 5. Remove the `latest_snapshot` Arc from `RecordWriter`

`RecordWriter<T>` in `aimdb-core/src/buffer/writer.rs` carries
`latest_snapshot: Arc<Mutex<Option<T>>>`. Its `push()` method updates it.
Both are removed:

```rust
// Before
pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    buffer: Arc<dyn DynBuffer<T>>,
    #[cfg(feature = "std")]
    latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,
    #[cfg(not(feature = "std"))]
    latest_snapshot: Arc<spin::Mutex<Option<T>>>,
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,
}

// After
pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    buffer: Arc<dyn DynBuffer<T>>,
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,
}
```

`RecordWriter::push()` becomes:

```rust
fn push(&self, value: T) {
    self.buffer.push(value);
    #[cfg(feature = "std")]
    self.metadata.mark_updated();
}
```

The two `#[cfg]` constructor variants collapse into one. The `writer_handle()`
method in `TypedRecord` no longer passes the snapshot Arc.

### 6. Merge the two divergent update paths

`TypedRecord::produce()` is deleted. All call sites migrate to going through
`RecordWriter::push()` (directly or via `Producer<T>`):

- `aimdb-core/src/builder.rs:1106` — body of the public
  `AimDb::produce<T>(key, value)`. Rewrite as
  `self.producer::<T>(key)?.produce(value)`. This is a per-call helper that
  performs a key lookup and constructs a fresh `Producer<T>` each call; the
  docstring already steers hot-path users to `db.producer::<T>(key)` once
  and reuse. `Database::produce()` (`database.rs:84`) and
  `aimdb-sync/src/handle.rs:407` go through this wrapper and need no
  further changes.
- `aimdb-core/src/typed_record.rs:1716` — inside
  `AnyRecord::set_from_json()`. Replace `self.produce(value)` with a push
  through `self.writer_handle()`:
  ```rust
  self.writer_handle().push(value);
  ```
  This keeps `set_from_json` synchronous and avoids constructing a
  `Producer<T>` purely to throw it away. `writer_handle()` is already
  `pub(crate)` and exists for this kind of internal use.
- `aimdb-wasm-adapter/src/bindings.rs:553` and `ws_bridge.rs:760` — both
  perform a `get_typed_record_by_key::<T, _>(key)` lookup per call and then
  call `typed.produce(val)`. Replace with `typed.writer_handle().push(val)`
  for the same reason (no behavioural change vs. a one-shot `Producer<T>`,
  but avoids the extra `Arc` clone). A future optimisation can cache a
  `Producer<T>` per known record key at adapter build time, but that is
  not required for this milestone.

After these migrations, `TypedRecord::produce()` has zero callers and is
removed. `RecordWriter::push()` is the single update path.

---

## Per-Buffer-Type Analysis

### SingleLatest (watch)

`TokioBufferInner::Watch { tx: watch::Sender<Option<T>> }` already holds the
value in its own internal slot. `tx.borrow()` returns a reference into that
slot without copying. `peek()` clones it out for the caller.

**Result:** zero duplication. The value exists exactly once — in the watch
sender. AimX `record.get` reads it via `peek()`. AimX `record.subscribe`
creates a `watch::Receiver` clone, which tracks its own version counter and
streams future changes via `recv()`. No extra allocation anywhere.

### Mailbox (notify + mutex slot)

`TokioBufferInner::Notify { slot: Arc<StdMutex<Option<T>>>, notify }` already
holds the value in `slot`. `peek()` locks and clones it — the same operation
the snapshot previously duplicated with a second mutex.

**Result:** zero duplication. The value exists exactly once — in the mailbox
slot. Removing the snapshot removes one of two identical mutexes.

Note: `peek()` on `Mailbox` is non-destructive (it clones without taking),
unlike `try_recv()` which calls `slot.take()`. AimX `record.get` uses `peek()`;
AimX `record.drain` uses `try_recv()` via the drain reader and consumes the
slot, matching drain semantics.

**Behaviour change vs. today.** Currently the snapshot is independent of the
mailbox slot, so a sequence `record.set` → `record.drain` → `record.get`
returns the value from snapshot on the final call. After this change, the
final `record.get` returns `not_found` because the drain already took the
slot. This matches Mailbox's documented "single-consumer take" semantics and
is the intended result, but it is a visible change for clients that interleave
`get` and `drain` on a Mailbox record.

### SPMC Ring (broadcast)

`TokioBufferInner::Broadcast { tx: broadcast::Sender<T> }` is a ring buffer
with no canonical "latest" concept. `peek()` returns `None`.

**For `record.get`:** returns `not_found`. See the explicit choice and
rationale in [§2](#2-aimx-recordget-uses-peek-directly).

**Result:** no duplication. The ring holds values in its own allocation;
nothing else stores them. Clients that need point-in-time reads on the
"latest produced" should use `SingleLatest`; clients that want the buffered
history should subscribe via `record.drain`.

### Bufferless records

Records with no buffer (passive/settable records that have no consumers and
no `BufferCfg`) have no native storage.

`peek()` on a `None` buffer returns `None`. `record.get` returns `not_found`.
`record.set` (write path) does not need to maintain any snapshot — it produces
via `RecordWriter` which pushes to the buffer. If a buffer is later added, the
next produced value becomes the first peekable value.

This is a semantic change: today `record.get` on a settable record returns its
last-set value via the snapshot. After this change it returns `not_found` until
a value has been produced AND a buffer is configured. If this use case needs
preserving, such records should use a `SingleLatest` buffer (one slot, zero
overhead beyond the watch sender).

---

## no\_std Impact

AimX (Unix socket, JSON protocol) is entirely `#[cfg(feature = "std")]`. No
no\_std code path ever calls `DynBuffer::peek()`, accesses `drain_readers`, or
needs point-in-time snapshot reads. The `spin::Mutex<Option<T>>` field is
used only:

1. On every `produce()` call — to update the snapshot (hot path, wasted work)
2. By `TypedRecord::latest()` — which is only called by std-gated code

Removing the field from the no\_std struct eliminates:

- One `spin::Mutex` acquisition per `produce()` on the hot path
- One `Option<T>` clone per `produce()` on the hot path
- ~10 `#[cfg(not(feature = "std"))]` blocks in `typed_record.rs` and `writer.rs`

The `spin` crate dependency is **not** removed by this milestone:
`producer_service`, `consumers`, and `transform` on `TypedRecord` still use
`spin::Mutex` in no\_std. Eliminating the dep would require unifying those
fields too — out of scope.

The Embassy adapter does not need to implement `DynBuffer::peek()`. The
default `fn peek(&self) -> Option<T> { None }` is correct and sufficient.

If embedded introspection is ever needed (e.g., reading current state without
subscribing a consumer), `embassy_sync::watch::Watch::receiver().try_get()`
provides it with zero additional storage.

---

## Implementation Plan

The steps below are ordered so the codebase compiles and tests pass at every
stage.

**Step 1 — Add `DynBuffer::peek()` with default `None`**  
Add the method to the trait in `aimdb-core/src/buffer/traits.rs`. No existing
code changes. All adapters inherit the default. Green.

**Step 2 — Implement `peek()` in `TokioBuffer`**  
Add `Watch` and `Notify` arms in `aimdb-tokio-adapter/src/buffer.rs`
(`Broadcast` uses the default `None`). Add unit tests for both. Green.

**Step 3 — Migrate `record.get` to `peek()`**  
Rewrite `AnyRecord::latest_json()` for `TypedRecord` to call
`self.buffer.as_ref()?.peek()` and serialize directly via the existing
`json_serializer` closure, bypassing `RecordValue<T>`. Keep `latest()` and
`RecordValue<T>` untouched — they remain on the typed in-process API and
still read the snapshot for now. `handle_record_get` in
`aimdb-core/src/remote/handler.rs` continues to call
`db.try_latest_as_json()`; the change is internal to `latest_json`.
Update/add integration tests for `record.get` on `SingleLatest`, `Mailbox`,
and SPMC Ring (the last asserting `not_found`). Green.

**Step 4 — Migrate all `TypedRecord::produce()` callers**  
This step must remove **every** caller before the field is deleted in Step 5,
otherwise Step 5 won't compile.

- `aimdb-core/src/typed_record.rs:1716` (`set_from_json`) — replace
  `self.produce(value)` with `self.writer_handle().push(value)`. The
  buffer-presence check immediately above this line stays.
- `aimdb-core/src/builder.rs:1106` (`AimDb::produce`) — rewrite the body as
  `self.producer::<T>(key)?.produce(value)`. `Database::produce` and
  `aimdb-sync/src/handle.rs` are transitive and need no edits.
- `aimdb-wasm-adapter/src/bindings.rs:553` and
  `aimdb-wasm-adapter/src/ws_bridge.rs:760` — replace `typed.produce(val)`
  with `typed.writer_handle().push(val)`.

After this step `TypedRecord::produce()` itself is still in place but unused
externally; leaving it for one more step keeps the diff focused.

**Step 5 — Remove `latest_snapshot` from `RecordWriter`**  
Remove the field. Remove the snapshot update from `push()`. Remove the
snapshot Arc parameter from `RecordWriter::new()` and from the
`writer_handle()` call site in `typed_record.rs`. Collapse the two `#[cfg]`
constructor variants into one. Green.

**Step 6 — Remove `latest_snapshot` from `TypedRecord`**  
Delete `TypedRecord::produce()` (now provably unused after Step 4). Remove
the `latest_snapshot` field and its constructor initialisers. Rewrite
`TypedRecord::latest()`:

```rust
pub fn latest(&self) -> Option<RecordValue<T>> {
    let value = self.buffer.as_ref()?.peek()?;
    #[cfg(feature = "std")]
    { Some(RecordValue::new(value, self.json_serializer.clone())) }
    #[cfg(not(feature = "std"))]
    { Some(RecordValue::new(value, None)) }
}
```

This makes `latest()` consistent with `latest_json()` (Step 3) — both read
buffer-native storage. Settable records with no buffer now return `None`
from `latest()`; see [Breaking Changes](#breaking-changes). `RecordValue<T>`
and its `json_serializer` Arc stay — they belong to the typed API and are
not part of this milestone. Green.

**Step 7 — Out of scope: `JsonRecord` supertrait**  
The `json_serializer` / `json_deserializer` closure fields on `TypedRecord`
still exist after this milestone. A follow-up proposal will replace them
with a blanket `impl<T: Serialize + DeserializeOwned>` `JsonRecord`
supertrait. Tracked separately.

---

## Breaking Changes

**`TypedRecord::latest()` semantics** — return type is unchanged
(`Option<RecordValue<T>>`), but the source is now `buffer.peek()`. Records
with no buffer (today: settable records that store only in the snapshot)
will return `None`. Mitigation: any settable record that needs `latest()`
or `record.get` to work must be configured with `SingleLatest` (one slot,
overhead is one `watch::Sender<Option<T>>` — strictly less than the snapshot
mutex it replaces). Audit `with_remote_access()` call sites at
implementation time and either attach a `SingleLatest` automatically in the
builder or fail loudly at `build()` if no buffer is configured for a
remote-readable record.

**`record.get` on SPMC Ring records** — today returns the most recently
produced value via the snapshot. After this change returns `not_found`.
Clients that need history use `record.drain`; clients that need
point-in-time reads should be configured with `SingleLatest`. See
[§2](#2-aimx-recordget-uses-peek-directly) and
[SPMC Ring §](#spmc-ring-broadcast).

**`record.get` on Mailbox after `record.drain`** — today survived a drain
via the independent snapshot; after this change a drained Mailbox slot is
empty and `record.get` returns `not_found` until the next produce. See
[Mailbox §](#mailbox-notify--mutex-slot).

**Metadata `last_update` on previously-divergent paths** — values produced
via `set_from_json` (AimX `record.set`), the public `AimDb::produce<T>`
wrapper, and the WASM adapter previously did not call
`RecordMetadataTracker::mark_updated()`. After unification all paths do.
Behaviour change is benign (metadata becomes more accurate) but is
observable via `record.metadata`.

**`RecordWriter::new()` signature** — internal `pub(crate)`; the
`latest_snapshot` Arc parameter is removed.

**`TypedRecord::produce()` removed** — internal. All call sites are
migrated in Step 4.

---

## Out of Scope

- **`JsonRecord` supertrait extraction** — removing `json_serializer` /
  `json_deserializer` stored closures in favour of blanket impls on
  `T: Serialize + DeserializeOwned`. `RecordValue<T>`'s `json_serializer`
  Arc lives or dies with that work; this milestone leaves it in place.

- **`std`/`no_std` Mutex unification across the remaining `TypedRecord`
  fields** (`producer_service`, `consumers`, `transform`). These still use
  `spin::Mutex` in no_std after this milestone, so the `spin` crate
  dependency is not removed. Tracked separately.

- **Embassy `peek()` implementation** — optional and additive; can be done
  as a follow-up when embedded introspection is required. The trait default
  `None` is correct.

- **Caching `Producer<T>` per record key in the WASM adapter** — Step 4
  uses a per-call `writer_handle()` for `bindings.rs`/`ws_bridge.rs` to keep
  the diff small. A future optimisation can pre-build a key→`Producer<T>`
  map at adapter init time.
