# Remove `Spawn` Trait — `build()` Collects, `run()` Drives

**Version:** 0.1 (draft)
**Status:** 📝 Design
**Issue:** [#88](https://github.com/aimdb-dev/aimdb/issues/88)
**Last Updated:** May 23, 2026
**Milestone:** M13 — Architectural clean-up

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Current Architecture — Audit](#current-architecture--audit)
  - [Where `Spawn` propagates today](#where-spawn-propagates-today)
  - [Embassy workaround](#embassy-workaround)
  - [WASM workaround](#wasm-workaround)
  - [The key audit finding](#the-key-audit-finding)
- [Proposed Design](#proposed-design)
  - [New public API](#new-public-api)
  - [Internal mechanics](#internal-mechanics)
  - [Connector futures](#connector-futures)
  - [Remote supervisor](#remote-supervisor)
- [Type-System Changes](#type-system-changes)
  - [Bound removals — summary table](#bound-removals--summary-table)
  - [unsafe impl Send/Sync analysis](#unsafe-impl-sendsync-analysis)
  - [R phantom-data after removal](#r-phantom-data-after-removal)
- [Platform-Specific Concerns](#platform-specific-concerns)
  - [Tokio (std)](#tokio-std)
  - [Embassy (no_std + alloc)](#embassy-nostd--alloc)
  - [WASM (wasm32-unknown-unknown)](#wasm-wasm32-unknown-unknown)
- [Shutdown / Cancellation](#shutdown--cancellation)
- [Connector API impact](#connector-api-impact)
- [Breaking Changes](#breaking-changes)
- [Implementation Plan](#implementation-plan)
- [Alternatives Considered](#alternatives-considered)
- [Open Questions](#open-questions)

---

## Summary

Remove `Spawn` from the `Runtime` bundle trait and from every `R: Spawn` bound
in `aimdb-core`. All futures that would previously be individually spawned
inside `build()` are instead **collected** into a `Vec<BoxFuture>`. A new
`AimDb::run()` method drives them via `FuturesUnordered`, blocking until
shutdown.

The core invariant that makes this safe: **all `spawn()` calls in AimDB happen
inside `build()`.** No task is ever spawned after initialisation completes
(with one narrow exception — the remote-access supervisor, discussed below).

---

## Motivation

### Problem 1 — Spawn permeates the type system

`Spawn` is currently a supertrait of `Runtime`:

```rust
pub trait Runtime: RuntimeAdapter + TimeOps + Logger + Spawn {}
```

Because `AimDb<R>` requires `R: Spawn`, and `AimDb<R>` is embedded in
`TypedRecord<T, R>`, `Producer<T, R>`, `Consumer<T, R>`, and
`RuntimeContext<R>`, a `Spawn` bound propagates to every layer. Adding a new
runtime adapter requires implementing `Spawn` even if the adapter never needs
to spawn dynamically.

### Problem 2 — Embassy task pool workaround

Embassy's task system requires statically-typed futures. To implement `Spawn`,
`aimdb-embassy-adapter` heap-allocates every future, type-erases it into a
`BoxedFuture`, and feeds it into a compile-time-fixed task pool via:

```rust
unsafe { Pin::new_unchecked(boxed_future) }
```

The pool size is selected with a feature flag (`embassy-task-pool-8/16/32`).
Choosing the wrong size panics at runtime. The `unsafe` block has no audit
trail.

### Problem 3 — unsafe impl Send/Sync

`EmbassyAdapter` wraps an `Option<Spawner>`. Because `Spawner` is `!Send`,
`EmbassyAdapter` is `!Send` without an `unsafe impl`. The adapter adds it to
satisfy `F: Send + 'static` demanded by the `Spawn` trait:

```rust
// SAFETY: Embassy executor handles spawner synchronization internally.
unsafe impl Send for EmbassyAdapter {}
unsafe impl Sync for EmbassyAdapter {}
```

`WasmAdapter` has the same pattern for WASM's single-threaded executor.
`Producer<T, R>` and `Consumer<T, R>` also carry `unsafe impl Send/Sync`
because they hold `Arc<AimDb<R>>` which transitively requires `R: Send + Sync`.

---

## Current Architecture — Audit

### Where `Spawn` propagates today

| Location | Bound | Role |
|---|---|---|
| `aimdb-executor/src/lib.rs` | `Runtime: … + Spawn` | bundle supertrait |
| `aimdb-core/src/builder.rs` | `AimDb<R: Spawn>`, `AimDbBuilder<R: Spawn>` | primary database types |
| `aimdb-core/src/builder.rs` | `AimDbInner::get_typed_record_by_key/id<R: Spawn>` | lookup helpers |
| `aimdb-core/src/typed_record.rs` | `TypedRecord<T, R: Spawn>` | per-record storage |
| `aimdb-core/src/typed_record.rs` | `RecordSpawner<T>::spawn_all_tasks<R: Spawn>` | spawn orchestrator |
| `aimdb-core/src/typed_record.rs` | `AnyRecordExt::as_typed<R: Spawn>` | downcast helper |
| `aimdb-core/src/typed_record.rs` | `spawn_producer_service`, `spawn_consumer_tasks`, `spawn_transform_task` | direct spawning |
| `aimdb-core/src/typed_api.rs` | `Producer<T, R: Spawn>`, `Consumer<T, R: Spawn>` | typed handles |
| `aimdb-core/src/transform/mod.rs` | `TransformDescriptor<T, R: Spawn>` | deferred transform |
| `aimdb-core/src/database.rs` | `Database<A: Spawn>` | high-level wrapper |
| `aimdb-core/src/remote/supervisor.rs` | `R: Spawn` | supervisor spawning |
| `aimdb-core/src/remote/handler.rs` | `R: Spawn` (×14 bounds) | handler dispatch |

### Embassy workaround

```
EmbassyAdapter::spawn(future: F)
  │
  ├─ Box::new(future)          // heap-allocate
  ├─ type-erase → BoxedFuture
  └─ TASK_POOL.spawn(          // fixed-size array
         unsafe { Pin::new_unchecked(boxed) }
     )
```

`TASK_POOL_SIZE` is selected at compile time via feature flags. If more tasks
are spawned than the pool size, the `spawn()` call panics. Choosing a pool
that is too large wastes RAM on constrained targets.

### WASM workaround

```rust
impl Spawn for WasmAdapter {
    fn spawn<F>(&self, future: F) -> ExecutorResult<()>
    where F: Future<Output = ()> + Send + 'static
    {
        wasm_bindgen_futures::spawn_local(future);
        Ok(())
    }
}
// Required because Spawn demands F: Send, but WASM is single-threaded:
unsafe impl Send for WasmAdapter {}
unsafe impl Sync for WasmAdapter {}
```

The `unsafe` exists solely to satisfy the `Spawn` trait's `F: Send` bound,
which is vacuously satisfied on single-threaded WASM.

### The key audit finding

All `spawn()` call sites in AimDB are inside `build()`:

| Call site | File | One future per… |
|---|---|---|
| `spawn_producer_service` | `typed_record.rs` | `.source()` |
| `spawn_consumer_tasks` | `typed_record.rs` | `.tap()` |
| `spawn_transform_task` | `typed_record.rs` | `.transform()` / `.transform_join()` |
| on_start tasks | `builder.rs` | `.on_start()` |
| Connector tasks | `builder.rs` (via `ConnectorBuilder::build`) | per-connector |
| Remote supervisor | `builder.rs` → `supervisor.rs` | one per `with_remote_access()` |

The total future count is **fully determined** by the database configuration at
the point `build()` is called. This is exactly the use case `FuturesUnordered`
is designed for.

> **Exception — per-connection handlers:** The remote-access supervisor spawns
> **per-connection** handlers at runtime using bare `tokio::spawn`. These are
> already outside the `Spawn` trait abstraction and are unaffected by this
> change. See [Remote supervisor](#remote-supervisor) below.
>
> **Exception — connector infrastructure tasks:** Each Tokio connector also
> spawns one permanent infrastructure task via bare `tokio::spawn` — entirely
> separate from the outbound-publisher tasks that go through `runtime.spawn()`:
>
> | Connector | Infrastructure task | Current call |
> |---|---|---|
> | MQTT (Tokio) | `rumqttc` EventLoop poll loop | `spawn_event_loop()` → `tokio::spawn` |
> | KNX (Tokio) | UDP connection + reconnect loop | `spawn_connection_task()` → `tokio::spawn` |
> | WebSocket | Axum server (`axum::serve`) | `start_server()` → `tokio::spawn` |
>
> Unlike the per-connection handlers, these infrastructure tasks **must be
> converted** to returned futures as part of Step 6. If left unchanged,
> connectors continue to call `tokio::spawn` directly and their infrastructure
> futures are invisible to `AimDbRunner`.

---

## Proposed Design

### New public API

```rust
// Build — returns a (handle, runner) pair
let (db, runner) = AimDbBuilder::new()
    .runtime(adapter)
    .configure::<Temperature>("sensor.temp", |reg| { ... })
    .build()
    .await?;

// db is a plain Clone-able handle; clone freely before starting the runner
let handle = db.clone();

// Run — drives all futures collected during build(), blocks until shutdown
runner.run().await;
```

`build()` returns `(AimDb<R>, AimDbRunner)`. `AimDb<R>` is an ordinary
clone-able handle (same as today). `AimDbRunner` is a non-`Clone` struct that
owned the collected futures; it has no `Arc` or `Mutex` wrapping.

`AimDbRunner::run()` takes `self` by value, consuming the futures vec.

### Internal mechanics

#### `build()` — collection phase

Replace every `runtime.spawn(future)` call with:

```rust
futures.push(future);
```

where `futures: Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>`.

At the end of `build()`, the vec is wrapped in `AimDbRunner` and returned
alongside the `AimDb<R>` handle:

```rust
/// Non-Clone runner returned by build().
/// Owns the complete set of futures that drive the database.
pub struct AimDbRunner {
    futures: Vec<BoxFuture<'static, ()>>,
}

// AimDb<R> itself is unchanged — no futures field, no Mutex.
pub struct AimDb<R: RuntimeAdapter + 'static> {
    inner: Arc<AimDbInner>,
    runtime: Arc<R>,
    // ... profiling, etc. — exactly as today
}
```

#### `run()` — driving phase

```rust
use futures::stream::{FuturesUnordered, StreamExt};

impl AimDbRunner {
    pub async fn run(self) {
        if self.futures.is_empty() {
            return; // nothing to drive
        }

        let mut set = FuturesUnordered::new();
        for f in self.futures {
            set.push(f);
        }

        // Drive all futures to completion (normally: forever, until shutdown)
        while set.next().await.is_some() {}
    }
}
```

`FuturesUnordered` polls each future cooperatively. Tasks that finish early
(e.g. a one-shot `on_start`) are dropped; tasks that run indefinitely (producer
services, consumer loops) keep the set alive.

### Connector futures

`ConnectorBuilder::build()` currently spawns its own tasks as a side effect.
With this change, connectors must **return** their futures instead of spawning
them. The `ConnectorBuilder` trait becomes:

```rust
pub trait ConnectorBuilder<R: RuntimeAdapter> {
    async fn build(
        self: Box<Self>,
        db: &AimDb<R>,
    ) -> DbResult<Vec<BoxFuture<'static, ()>>>;
}
```

`AimDbBuilder::build()` collects the returned futures and adds them to the
accumulator. This is a **breaking change** to the `ConnectorBuilder` trait
(connector authors must update `build()`).

#### Two layers of futures per connector

Each connector currently has two distinct categories of futures, both of which
must be returned:

1. **Outbound publisher futures** — one per `link_to()` route, currently
   spawned via `runtime.spawn()` inside `spawn_outbound_publishers()`. These
   subscribe to a typed record and publish serialised values to the external
   system.

2. **Infrastructure futures** — one per connector instance, currently spawned
   via bare `tokio::spawn()` inside internal helpers (see audit table above).
   Converting these is covered in Step 6b.

#### The dropped `Arc<dyn Connector>` object

The current `ConnectorBuilder::build()` returns `Arc<dyn Connector>`, but
`builder.rs` already discards this value immediately:
`let _connector = builder.build(&db).await?;`. The `Connector::publish()`
method — used for direct programmatic publishing — is already inaccessible
through the `AimDbBuilder` public API. Changing the return type to
`Vec<BoxFuture>` causes no behavioural regression here.

Connector implementations keep their data alive via `Arc` clones captured
inside the returned futures (e.g. `Arc<AsyncClient>` in MQTT,
`mpsc::Sender<KnxCommand>` in KNX) — not via the connector object itself.

#### KNX channel ownership ordering

The KNX connector creates an `mpsc::channel` inside `spawn_connection_task()`:
the receiver is captured by the connection task, and the sender is cloned into
each outbound publisher task. In the new model the channel must be created
before either set of futures is constructed:

```rust
// 1. Create channel first
let (cmd_tx, cmd_rx) = mpsc::channel(16);

// 2. Connection future captures cmd_rx
let connection_future: BoxFuture<'static, ()> = Box::pin(async move {
    // UDP connection + reconnect loop, reads from cmd_rx
});

// 3. Outbound publisher futures each clone cmd_tx
let publisher_futures: Vec<BoxFuture<'static, ()>> = routes
    .into_iter()
    .map(|route| {
        let tx = cmd_tx.clone();
        Box::pin(async move { /* subscribe → serialize → tx.send() */ }) as BoxFuture<'static, ()>
    })
    .collect();

// 4. All futures returned together
Ok(std::iter::once(connection_future).chain(publisher_futures).collect())
```

This ordering is already implicit in the current code and must be preserved
explicitly when refactoring.

### Remote supervisor

`supervisor::spawn_supervisor()` currently calls `runtime.spawn(supervisor_loop)`.
In the new model it returns the supervisor future instead:

```rust
pub fn build_supervisor<R: RuntimeAdapter>(
    db: Arc<AimDb<R>>,
    config: AimxConfig,
) -> DbResult<BoxFuture<'static, ()>>
```

The supervisor loop internally calls `tokio::spawn` for per-connection handlers
— this is a Tokio-specific behaviour that already bypasses the `Spawn` trait
and is **unchanged** by this refactor. Per-connection spawning happens inside a
future that is driven by `FuturesUnordered`, which is fine because
`FuturesUnordered` is poll-based: the supervisor future yields control between
accepted connections.

---

## Type-System Changes

### Bound removals — summary table

| Type / fn | Before | After |
|---|---|---|
| `Runtime` supertrait | `RuntimeAdapter + TimeOps + Logger + Spawn` | `RuntimeAdapter + TimeOps + Logger` |
| `AimDb<R>` | `R: Spawn + 'static` | `R: RuntimeAdapter + 'static` |
| `AimDbBuilder<R>` | `R: Spawn + 'static` | `R: RuntimeAdapter + 'static` |
| `TypedRecord<T, R>` | `R: Spawn + 'static` | `R: 'static` |
| `Producer<T, R>` | `R: Spawn + 'static` | `R: 'static` |
| `Consumer<T, R>` | `R: Spawn + 'static` | `R: 'static` |
| `TransformDescriptor<T, R>` | `R: Spawn + 'static` | `R: 'static` |
| `RecordSpawner::spawn_all_tasks<R>` | `R: Spawn + 'static` | removed (see below) |
| `AnyRecordExt::as_typed<R>` | `R: Spawn + 'static` | `R: 'static` |
| `Database<A>` | `A: Spawn + 'static` | `A: RuntimeAdapter + 'static` |
| `remote/handler.rs` (×14) | `R: Spawn + 'static` | `R: RuntimeAdapter + 'static` |
| `remote/supervisor.rs` | `R: Spawn + 'static` | `R: RuntimeAdapter + 'static` |

### `RecordSpawner` → `RecordFutureCollector`

`RecordSpawner<T>::spawn_all_tasks<R>()` currently calls `runtime.spawn()` for
each task. It is renamed and refactored to **collect** futures instead:

```rust
pub struct RecordFutureCollector<T> { _phantom: PhantomData<T> }

impl<T: Send + Sync + 'static + Debug + Clone> RecordFutureCollector<T> {
    pub fn collect_all_futures<R: 'static>(
        record: &dyn AnyRecord,
        db: &Arc<AimDb<R>>,
        record_key: &str,
    ) -> DbResult<Vec<BoxFuture<'static, ()>>> {
        let mut futures = Vec::new();
        // ... downcast, collect producer, transform, consumer futures ...
        Ok(futures)
    }
}
```

Similarly, `TypedRecord::spawn_producer_service` → `collect_producer_future`,
`spawn_consumer_tasks` → `collect_consumer_futures`,
`spawn_transform_task` → `collect_transform_future`.

### `unsafe impl Send/Sync` analysis

**`Producer<T, R>` and `Consumer<T, R>`:**

Currently marked `unsafe impl Send/Sync` because `R: Spawn` does not guarantee
`R: Send + Sync` on all platforms (notably Embassy and WASM). After this
change, `R: RuntimeAdapter` (which is `Send + Sync + 'static`), and
`Arc<AimDb<R>>` is `Send + Sync` if `R: Send + Sync`. Since `RuntimeAdapter`
already requires `Send + Sync + 'static`, `Producer<T, R>` and `Consumer<T, R>`
will auto-derive `Send + Sync` without `unsafe`. **Remove the unsafe impls.**

**`EmbassyAdapter`:**

`EmbassyAdapter` currently holds `Option<Spawner>`. `Spawner` is `!Send`.
With `Spawn` removed, the `Spawner` field is no longer needed (it was only
used in `impl Spawn for EmbassyAdapter`). **Remove the `spawner` field.**

After removing `spawner`, `EmbassyAdapter` becomes a simple struct holding
only an `Option<&'static Stack<'static>>` (the network stack, gated by
`embassy-net-support`). `&'static Stack<'static>` is `Send + Sync`, so
`EmbassyAdapter` auto-derives `Send + Sync`. **Remove the `unsafe impl`.**

The `new_with_spawner()` constructor is removed (breaking change — see
[Breaking Changes](#breaking-changes)).

**`WasmAdapter`:**

`WasmAdapter` is already a ZST (`#[derive(Clone, Copy, Debug)]` with no
fields). With `Spawn` removed it no longer needs the `unsafe impl`. Since it is
a ZST it auto-derives `Send + Sync`. **Remove the `unsafe impl`.**

**Embassy connector futures:**

Embassy connector implementations (e.g. the Embassy variant of
`aimdb-mqtt-connector`) currently wrap their outbound publisher futures in
`SendFutureWrapper` to satisfy the `F: Send + 'static` bound imposed by
`impl Spawn for EmbassyAdapter`. With `Spawn` removed, `runtime.spawn()` calls
disappear — but `Vec<BoxFuture<'static, ()>>` (where
`BoxFuture = Pin<Box<dyn Future + Send + 'static>>`) still requires `Send`.
Embassy futures are `!Send`. Embassy connector implementations must therefore
still wrap their returned futures in `SendFutureWrapper` before pushing them
to the Vec. The `unsafe` burden shifts from the adapter to the connectors —
the total number of `unsafe impl` blocks decreases, but is not reduced to zero.

### R phantom-data after removal

`Producer<T, R>` and `Consumer<T, R>` currently hold `Arc<AimDb<R>>` and bind
`R` for type inference at call sites (`producer.produce(val).await`). After the
bound change, `R` is still carried but is now only constrained to `R: 'static`.
The `_phantom: PhantomData<T>` field remains as-is.

Whether to collapse `R` entirely (making `Producer<T>`) is a larger question
deferred to a follow-up refactor — it would change all public API signatures.

---

## Platform-Specific Concerns

### Tokio (std)

`FuturesUnordered` from `futures-util` is the natural choice. Tokio users call:

```rust
tokio::select! {
    _ = runner.run()        => {},
    _ = shutdown_signal()   => {},
}
```

No adapter changes beyond removing `impl Spawn for TokioAdapter`.

### Embassy (no_std + alloc)

`FuturesUnordered` from `futures-util` compiles in `no_std + alloc` mode
(requires `futures-util` with `default-features = false, features = ["alloc"]`).

Embassy main becomes:

```rust
#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let adapter = EmbassyAdapter::new().unwrap();
    let db = AimDbBuilder::new()
        .runtime(Arc::new(adapter))
        .configure::<Temperature>(...)
        .build()
        .await
        .unwrap();

    runner.run().await; // drives FuturesUnordered inside the Embassy main task
}
```

Embassy's cooperative scheduler handles the `FuturesUnordered::next().await`
poll loop the same way it handles any `await` point. This replaces the task
pool entirely.

**Memory profile:** Each boxed future occupies one heap allocation.  The total
number of futures is bounded by the database configuration, same as today.
Heap usage is therefore unchanged.

**Pool size feature flags** (`embassy-task-pool-8/16/32`) are **deleted**.

### WASM (wasm32-unknown-unknown)

WASM's single-threaded runtime presents no new concerns because
`FuturesUnordered` works on single-threaded executors. `runner.run()` is
awaited inside the WASM main function or a `wasm_bindgen_futures::spawn_local`
root.

---

## Shutdown / Cancellation

`FuturesUnordered::next()` returns `None` only when all futures have completed.
Producer services and consumer loops are infinite by design; `run()` therefore
blocks for the application's lifetime.

Cancellation strategies:
1. **Tokio `CancellationToken`** — wrap `db.run()` in `select!` with a
   cancellation token. Producer/consumer tasks should check the token in their
   loop body.
2. **CTRL-C / signal handler** — same `select!` pattern with
   `tokio::signal::ctrl_c()`.
3. **Embassy** — cooperative via task cancellation or a shared `AtomicBool`
   stop flag.

The design does not prescribe a shutdown mechanism; that is the application's
responsibility. A future enhancement could add a `db.shutdown()` method that
drops the futures set.

---

## Connector API impact

Connectors that implement `ConnectorBuilder` must be updated:

**Before:**
```rust
async fn build(self: Box<Self>, db: &AimDb<R>) -> DbResult<Box<dyn Connector>> {
    // spawns tasks internally via runtime.spawn()
    runtime.spawn(my_task_future);
    Ok(Box::new(MyConnector { ... }))
}
```

**After:**
```rust
async fn build(
    self: Box<Self>,
    db: &AimDb<R>,
) -> DbResult<Vec<BoxFuture<'static, ()>>> {
    // returns futures for AimDbBuilder to collect
    Ok(vec![Box::pin(my_task_future)])
}
```

Affected connectors: `aimdb-mqtt-connector` (Tokio and Embassy), `aimdb-knx-connector`,
`aimdb-websocket-connector`, and aimdb-pro call sites.

---

## Breaking Changes

| Area | Change |
|---|---|
| Public API | `db.run().await` must be called after `build()` — tasks do not start until `run()` |
| `Runtime` supertrait | `Spawn` removed — custom adapters no longer need to implement it |
| `EmbassyAdapter` | `new_with_spawner(spawner)` constructor removed |
| `EmbassyAdapter` feature flags | `embassy-task-pool-8/16/32` removed |
| `ConnectorBuilder` trait | `build()` now returns `Vec<BoxFuture>` instead of `Box<dyn Connector>` |
| `spawn_fns` in `AimDbBuilder` | internal — no public API change, but internal structure changes |

---

## Implementation Plan

Listed in dependency order. Each step should pass `make check` before the next
begins.

### Step 1 — Executor layer

**File:** `aimdb-executor/src/lib.rs`

- Remove `Spawn` from the `Runtime` supertrait.
- Delete the `Spawn` trait entirely from `aimdb-executor`.
- Remove the `SpawnFailed` variant from `ExecutorError`.
- Add `futures-util` dependency with `default-features = false, features = ["alloc"]`.

**Test:** `cargo check --all-features` + `cargo check --target thumbv7em-none-eabihf`.

---

### Step 2 — Core: loosen bounds in typed_api and typed_record

**Files:** `aimdb-core/src/typed_api.rs`, `aimdb-core/src/typed_record.rs`,
`aimdb-core/src/transform/mod.rs`

- Remove `R: Spawn` bound from `Producer<T, R>`, `Consumer<T, R>`, `RecordRegistrar`.
- Remove `unsafe impl Send/Sync` from `Producer<T, R>` and `Consumer<T, R>` —
  verify auto-derivation works.
- Remove `R: Spawn` bound from `TypedRecord<T, R>` and `TransformDescriptor<T, R>`.
- Remove `R: Spawn` bound from `AnyRecordExt::as_typed<R>`.

---

### Step 3 — Core: refactor spawn → collect

**File:** `aimdb-core/src/typed_record.rs`

Rename and refactor:
- `RecordSpawner<T>` → `RecordFutureCollector<T>`
- `spawn_all_tasks` → `collect_all_futures` (returns `Vec<BoxFuture>`)
- `spawn_producer_service` → `collect_producer_future` (returns `Option<BoxFuture>`)
- `spawn_consumer_tasks` → `collect_consumer_futures` (returns `Vec<BoxFuture>`)
- `spawn_transform_task` → `collect_transform_future` (returns `Option<BoxFuture>`)

Each method constructs the future and returns it rather than calling
`runtime.spawn()`.

---

### Step 4 — Core: `build()` accumulates, `run()` drives

**File:** `aimdb-core/src/builder.rs`

- Add `AimDbRunner` struct with a `futures: Vec<BoxFuture<'static, ()>>` field.
- Change `build()` return type from `DbResult<AimDb<R>>` to
  `DbResult<(AimDb<R>, AimDbRunner)>`.
- In `build()`, replace each `runtime.spawn(f)` with `futures_acc.push(f)`.
- At end of `build()`, wrap the accumulated vec in `AimDbRunner` and return
  it alongside the `AimDb<R>` handle.
- Implement `AimDbRunner::run(self)` using `FuturesUnordered`.
- `AimDb<R>` itself gains no new fields — it remains a plain clone-able handle.
- Remove `R: Spawn` bound from `AimDb<R>`, `AimDbBuilder<R>`,
  `AimDbInner::get_typed_record_by_key/id`.
- Collapse the `std`/`no_std` `on_start` bifurcation: unify
  `StartFnType<R>` and `NoStdStartFnType<R>` into a single alias
  `type StartFnType<R> = Box<dyn FnOnce(Arc<R>) -> BoxFuture<'static, ()>>;`
  (drop `+ Send` from the `FnOnce` — the closure is called once during
  `build()`, not moved across threads; `Send` is already required by the
  `BoxFuture` output type). Remove the `#[cfg(feature = "std")]` split.

---

### Step 5 — Core: remote access bounds

**Files:** `aimdb-core/src/remote/supervisor.rs`,
`aimdb-core/src/remote/handler.rs`

- Rename `spawn_supervisor` → `build_supervisor_future`; return
  `DbResult<BoxFuture<'static, ()>>` instead of spawning.
- Remove `R: Spawn` bounds from all 14 handler function signatures; replace
  with `R: RuntimeAdapter`.
- `build()` in `builder.rs` calls `build_supervisor_future()` and pushes the
  returned future to the accumulator.

---

### Step 6 — Connector API

**Files:** `aimdb-core/src/connector.rs`, all connector crates

#### 6a — Update trait signature and builder

- Update `ConnectorBuilder::build()` return type to
  `DbResult<Vec<BoxFuture<'static, ()>>>`.
- Remove `R: Spawn` bound from the `ConnectorBuilder<R>` trait definition.
- Update `builder.rs` to extend the accumulator with the returned Vec.

#### 6b — Convert infrastructure `tokio::spawn` calls

Each Tokio connector has one permanent infrastructure future currently spawned
via bare `tokio::spawn`. Convert each to construct and return a `BoxFuture`
instead:

- **MQTT:** `spawn_event_loop()` → `build_event_loop_future()` — return the
  `rumqttc` EventLoop poll loop as a `BoxFuture` instead of calling
  `tokio::spawn`.
- **KNX:** `spawn_connection_task()` → `build_connection_future()` — return
  `(BoxFuture<'static, ()>, mpsc::Sender<KnxCommand>)`. The `Sender` is then
  cloned into outbound publisher futures (see [KNX channel ownership
  ordering](#knx-channel-ownership-ordering) in the Connector futures section).
- **WebSocket:** `start_server()` → `build_server_future()` — return the
  `axum::serve(...)` loop as a `BoxFuture`. Per-connection tasks that Axum
  spawns internally remain as `tokio::spawn` — same category as the remote
  supervisor's per-connection handlers.

#### 6c — Replace outbound publisher spawn with collect

Replace `spawn_outbound_publishers()` with `collect_outbound_futures()` on each
connector implementation, returning `Vec<BoxFuture>` instead of calling
`runtime.spawn()` per route.

#### 6d — Embassy connector `SendFutureWrapper`

Embassy connector implementations must wrap returned futures in
`SendFutureWrapper` before pushing them to the Vec (Embassy futures are `!Send`
but `BoxFuture<'static, ()>` requires `Send`). This is the same pattern already
used on the Embassy path — the wrapping moves from the spawn call site to the
collection point.

---

### Step 7 — Adapter: Tokio

**File:** `aimdb-tokio-adapter/src/runtime.rs`

- Remove `impl Spawn for TokioAdapter`.

---

### Step 8 — Adapter: Embassy

**File:** `aimdb-embassy-adapter/src/runtime.rs`

- Remove `impl Spawn for EmbassyAdapter`.
- Remove `spawner: Option<Spawner>` field.
- Remove `new_with_spawner()` constructor.
- Remove `generic_task_runner`, `BoxedFuture`, task pool declarations.
- Remove `embassy-task-pool-8/16/32` Cargo features.
- Remove `unsafe impl Send for EmbassyAdapter` and `unsafe impl Sync for EmbassyAdapter`.
- Remove `build.rs` logic that configures task pool size (if applicable).

---

### Step 9 — Adapter: WASM

**File:** `aimdb-wasm-adapter/src/runtime.rs`

- Remove `impl Spawn for WasmAdapter`.
- Remove `unsafe impl Send for WasmAdapter` and `unsafe impl Sync for WasmAdapter`.

---

### Step 10 — Examples and aimdb-pro call sites

Update all examples to add `db.run().await` after `build()`. Update aimdb-pro
demo binaries, connectors, and any internal tooling that calls `build()`.

---

### Step 11 — Changelog and API docs

- Add entry to `CHANGELOG.md` documenting breaking changes.
- Update `README.md` quickstart snippet.
- Update doc comments on `build()` and `run()`.

---

## Alternatives Considered

### A — Keep `Spawn` on `Runtime`, drive with a shared `JoinSet`

`build()` could return a `JoinSet<()>` that callers await. This keeps the
`Spawn` trait but adds a handle-based shutdown mechanism. **Rejected** because
`JoinSet` is Tokio-specific and doesn't help Embassy or WASM.

### B — Static dispatch via `collect()` + join macro

Embassy already uses `join_array` / `select_array`. A macro could call the
right primitive per platform. **Rejected** because it requires platform-specific
`run()` implementations and adds conditional compilation complexity.

### C — Remove `R` from `Producer<T>` and `Consumer<T>` now

Collapsing `R` out of the typed handles would simplify the API further.
**Deferred** — it is a larger change to all connector and user-facing APIs.
Can be done in a follow-up milestone once this foundation is in place.

### D — Dual API: keep `spawn()` as an escape hatch

Expose `db.spawn(future)` for callers who legitimately need post-build task
spawning (e.g. a custom connector). **Deferred** — no current use case inside
the AimDB codebase requires it. Can be re-introduced if a concrete need arises,
backed by an explicit `Spawn` impl on the adapter.

---

## Decisions

All design questions raised during review have been resolved:

1. **`build()` return type** → **Tuple `(AimDb<R>, AimDbRunner)`.**
   Consistent with Rust channel idioms (`mpsc::channel`, `oneshot::channel`).
   No extra type to import; destructuring at every call site is idiomatic.

2. **`Spawn` trait retention** → **Delete entirely.**
   This is already a semver-breaking release; no deprecation cycle is needed.
   Any downstream crate that implemented `Spawn` must update for the other
   breaking changes regardless.

3. **`ExecutorError::SpawnFailed` variant** → **Remove.**
   With `Spawn` deleted, `SpawnFailed` is unreachable. Remove it in the same
   commit as the trait deletion (Step 1).

4. **`Database<A>` promotion** → **Defer.**
   Drop the `A: Spawn` bound as part of this milestone but keep `Database<A>`
   as a convenience wrapper. Renaming the primary user-facing type is a larger
   mechanical change with no functional benefit at this stage.

5. **Connector `build()` return type** → **`Vec<BoxFuture>` only.**
   `builder.rs` already discards `Arc<dyn Connector>` today (`let _connector
   = ...`). No behavioural regression. Introspection handles can be
   reconsidered in a future milestone when a concrete use case exists.

6. **`on_start` std/no_std bifurcation** → **Collapse.**
   Unify into a single `StartFnType<R>` alias (see Step 4 for details).
