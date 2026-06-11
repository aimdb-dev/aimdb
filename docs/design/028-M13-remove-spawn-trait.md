# Remove `Spawn` Trait — `build()` Collects, `run()` Drives

**Version:** 0.4 (Group 4 / AimX bridge state removed by design 030)
**Status:** ✅ Implemented
**Issue:** [#88](https://github.com/aimdb-dev/aimdb/issues/88)
**Follow-up:** [Design 030 — AimX remote-access spawn-free](030-M13-aimx-remote-spawn-free.md) / [#114](https://github.com/aimdb-dev/aimdb/issues/114) — ✅ Implemented
**Last Updated:** May 26, 2026
**Milestone:** M13 — Architectural clean-up

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Current Architecture — Audit](#current-architecture--audit)
  - [Where `Spawn` propagates today](#where-spawn-propagates-today)
  - [Embassy workaround](#embassy-workaround)
  - [WASM workaround](#wasm-workaround)
  - [Runtime spawn sites — full inventory](#runtime-spawn-sites--full-inventory)
- [Proposed Design](#proposed-design)
  - [New public API](#new-public-api)
  - [Internal mechanics](#internal-mechanics)
  - [Connector futures](#connector-futures)
  - [Remote supervisor](#remote-supervisor)
  - [`AimDb::spawn_task` deletion](#aimdbspawn_task-deletion)
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
  - [Other affected crates](#other-affected-crates)
- [Breaking Changes](#breaking-changes)
- [Implementation Plan](#implementation-plan)
- [Alternatives Considered](#alternatives-considered)
- [Decisions](#decisions)
- [Out of Scope](#out-of-scope)

---

## Summary

Remove `Spawn` from the `Runtime` bundle trait and from every `R: Spawn` bound
in `aimdb-core`. All futures that would previously be individually spawned
inside `build()` are instead **collected** into a `Vec<BoxFuture>`. A new
`AimDb::run()` method drives them via `FuturesUnordered`, blocking until
shutdown.

The intended invariant: **all `spawn()` calls reachable from `aimdb-core` happen
inside `build()` after this refactor.** A code audit (v0.2 of this doc)
identified five categories of runtime-spawn sites; the table in
[Runtime spawn sites — full inventory](#runtime-spawn-sites--full-inventory)
catalogues each and assigns it to one of: *converted to build-time
collection*, *deleted as part of this PR*, or *deferred to a follow-up
issue*.

> **Scope note (v0.2):** The v0.1 framing of "one narrow exception" was an
> oversimplification. The AimX remote-access spawn calls (per-connection
> handler + per-subscription stream) are non-trivial to remove because they
> are inherently dynamic-fan-out. Rather than couple their refactor to the
> `Spawn`-trait removal, this design **defers** them to a separate issue
> ([AimX remote-access portability](../issues/aimx-remote-spawn-free.md))
> so M13 can land as a focused trait-removal change. Within this PR, the AimX
> path keeps using bare `tokio::spawn` internally — which is fine because
> AimX is already `#[cfg(feature = "std")]`-gated. The follow-up issue both
> removes those spawn calls and prepares AimX for eventual un-gating.

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
| `aimdb-codegen/src/rust.rs` | emits `<R: Spawn + 'static>` into generated `configure_schema` | code generation |
| `aimdb-persistence/src/builder_ext.rs` | `R: Spawn + TimeOps` | persistence builder extension trait |
| `aimdb-persistence/src/ext.rs` | `R: Spawn + 'static` (×2) | persistence trait bounds |
| `aimdb-persistence/src/query_ext.rs` | `R: Spawn + 'static` (×2) | query trait + backend helper |
| `aimdb-sync/src/handle.rs` | `R: Spawn` | sync handle bounds |
| `aimdb-tokio-adapter/src/connector.rs` | `TokioAdapter::spawn_connectors` (test-only helper) | unused helper, deletable |

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

### Runtime spawn sites — full inventory

The v0.1 audit listed six build-time spawn sites and claimed they were the
total. A line-by-line code search produced the following complete picture.
Sites are grouped by disposition (what this PR does with them).

**Group 1 — Build-time spawns converted to returned futures (in scope).**

| Call site | File | One future per… |
|---|---|---|
| `spawn_producer_service` | `aimdb-core/src/typed_record.rs:1228` | `.source()` |
| `spawn_consumer_tasks` | `aimdb-core/src/typed_record.rs:1155` | `.tap()` |
| `spawn_transform_task` | `aimdb-core/src/typed_record.rs:881` | `.transform()` / `.transform_join()` |
| on_start tasks | `aimdb-core/src/builder.rs:977,988` | `.on_start()` |
| Connector outbound publishers | per-connector `spawn_outbound_publishers()` | `.link_to()` route |
| Connector infrastructure | MQTT `spawn_event_loop()`, KNX `spawn_connection_task()`, WS `start_server()` | one per connector |
| Remote supervisor entry point | `aimdb-core/src/builder.rs` → `supervisor.rs:108` | one per `with_remote_access()` |

**Group 2 — Runtime spawn that hoists to build time (in scope, new in v0.2).**

| Call site | File | Why it's actually a build-time fan-out |
|---|---|---|
| Join transform forwarders | `aimdb-core/src/transform/join.rs:329` | `inputs.len()` is fixed at `transform_join()` registration; lazy spawn is incidental |

The forwarder count is statically known when the `JoinPipeline` is built.
Step 3a converts the lazy spawn into build-time collection alongside the
join transform future itself.

**Group 3 — Runtime spawn deleted as part of this PR (in scope).**

| Item | File | Disposition |
|---|---|---|
| `AimDb::spawn_task` public method | `aimdb-core/src/builder.rs:1096` | Delete. With `Spawn` gone there is no portable backing primitive; the method has no internal callers. |
| `TokioAdapter::spawn_connectors` | `aimdb-tokio-adapter/src/connector.rs:54` | Delete. Test-only helper, no production callers. |
| `BufferOps::spawn_dispatcher` | `aimdb-tokio-adapter/src/buffer.rs:205` | Keep (test-only utility), but mark for removal in a follow-up tidy if no external user adopts it. |

**Group 4 — Runtime spawn deferred to follow-up issue (now resolved).**

All three sites were addressed by the AimX spawn-free follow-up
([design 030](030-M13-aimx-remote-spawn-free.md), issue
[#114](https://github.com/aimdb-dev/aimdb/issues/114)). Each was
converted to a nested `FuturesUnordered` driven by `tokio::select! {
biased; }`; cancellation collapsed to dropping the future.

| Call site | File | Resolution |
|---|---|---|
| AimX per-connection handler | `aimdb-core/src/remote/supervisor.rs` | Supervisor owns a `FuturesUnordered<BoxFuture>`; accepted connections are pushed in. |
| AimX per-subscription stream | `aimdb-core/src/remote/handler.rs` + `builder.rs` | New `stream_record_updates` helper returns a `Stream`; per-conn `FuturesUnordered` holds one future per `record.subscribe`. `subscribe_record_updates` deleted. |
| WebSocket **client** reconnect | `aimdb-websocket-connector/src/client/connector.rs` | Six `tokio::spawn` sites collapsed into one connector future that owns a `FuturesUnordered`; reconnect watcher sends `NewLoops` over an mpsc rather than spawning. |

**Group 5 — External / out-of-codebase (informational).**

`wasm_bindgen_futures::spawn_local` calls in `aimdb-wasm-adapter/src/{ws_bridge.rs,bindings.rs}` are WASM-runtime glue invoked by JS callbacks; they are outside the `Spawn` trait surface and unaffected. Example binaries and tests that call `tokio::spawn` directly are user code, not core.

#### Why these groupings make the refactor safe

Groups 1, 2, and 3 cover **every** `runtime.spawn(...)` call within
`aimdb-core` and every connector — once they land, the `Spawn` trait has no
internal callers. Group 4 retained bare `tokio::spawn` calls inside
`aimdb-core/src/remote/` as a deliberate bridge state in this PR; those
calls did **not** depend on the trait (they called Tokio directly through
`#[cfg(feature = "std")]`), so the trait could be deleted cleanly. The
follow-up ([design 030](030-M13-aimx-remote-spawn-free.md)) has since
removed every Group 4 spawn call.

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

**Target state achieved.** The bridge state described in v0.3 of this
design has been removed by the AimX spawn-free follow-up
([design 030](030-M13-aimx-remote-spawn-free.md), issue
[#114](https://github.com/aimdb-dev/aimdb/issues/114)). The supervisor
now pushes per-connection handler futures onto its own
`FuturesUnordered`; the handler does the same with per-subscription
futures backed by a `Stream`-returning helper (`stream_record_updates`);
`AimDb::subscribe_record_updates` is deleted. No `tokio::spawn` remains
in `aimdb-core/src/remote/`.

The AimX path is now runtime-agnostic in shape (still `#[cfg(feature =
"std")]`-gated for transport reasons). Lifting the `std` gate itself
remains a separate, larger effort; see design 030 §"Out of Scope" for
the remaining work.

### `AimDb::spawn_task` deletion

[`AimDb::spawn_task`](../../aimdb-core/src/builder.rs#L1096) is a public
convenience that forwards to `R::spawn`. With the trait removed there is
no portable backing primitive, and the method has no internal callers.
**Deleted in this PR** (Step 4). Downstream code that wants post-build task
creation must either:

1. Register the future via `on_start()` (preferred — collected by `build()`).
2. Place a `FuturesUnordered` inside its own future and push children there.

There is no third option. This is intentional: the surface area we are
removing is exactly the surface area that made the trait viral.

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
| `impl Clone for AimDb<R>` | `R: Spawn + 'static` | `R: RuntimeAdapter + 'static` |
| `aimdb-codegen/src/rust.rs` emitted code | `<R: Spawn + 'static>` | `<R: RuntimeAdapter + 'static>` (golden tests update) |
| `aimdb-persistence` (3 files) | `R: Spawn (+ TimeOps)` | `R: RuntimeAdapter (+ TimeOps)` |
| `aimdb-sync/src/handle.rs` | `R: Spawn` | `R: RuntimeAdapter` |

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
`embassy-net-support`).

> **Implementation note (v0.3).** The earlier draft assumed
> `&'static Stack<'static>` would be `Send + Sync` and the `unsafe impl`
> blocks could be deleted outright. In practice `embassy_net::Stack`
> contains a `RefCell` and is `!Sync`, so the adapter still needs:
>
> ```rust
> // SAFETY: Embassy executors run cooperatively on a single core. The
> // `embassy_net::Stack` (when present) is `!Sync` only because of its
> // internal `RefCell`; in a single-threaded executor no concurrent access
> // is possible.
> unsafe impl Send for EmbassyAdapter {}
> unsafe impl Sync for EmbassyAdapter {}
> ```
>
> The *justification* changes (single-threaded cooperative executor, no
> longer "satisfy the `Spawn` trait's `F: Send` bound") and the audit-trail
> comment is rewritten accordingly, but the `unsafe` blocks themselves
> stay. The net `unsafe`-block count on the adapter goes from
> [Send, Sync, Pin::new_unchecked in task pool] to just [Send, Sync] —
> the task-pool `Pin::new_unchecked` is the one that actually disappears.

The `new_with_spawner()` constructor is removed (breaking change — see
[Breaking Changes](#breaking-changes)).

`new_with_network(spawner, network)` ([runtime.rs:162](../../aimdb-embassy-adapter/src/runtime.rs#L162))
currently also takes a `Spawner`. With `Spawn` removed the parameter is dead
weight. The signature changes to `new_with_network(network)`. This breaks
three example binaries and any aimdb-pro snippets that pass `spawner`:

| Affected file | Current call |
|---|---|
| `examples/embassy-knx-connector-demo/src/main.rs:253` | `EmbassyAdapter::new_with_network(spawner, stack)` |
| `examples/embassy-mqtt-connector-demo/src/main.rs:317` | same |
| `examples/weather-mesh-demo/weather-station-gamma/src/main.rs:259` | same |
| aimdb-pro UI docs (llms-full.txt, 04-deployment.md) | same |

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
    let adapter = EmbassyAdapter::new();
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

**Cooperative-scheduling implication.** Today each AimDB future runs in its
own Embassy task with its own stack: a producer that blocks between awaits
only starves itself. After this change, **all** collected futures share a
single Embassy task's stack and yield budget. In practice this is
benign — AimDB futures are async I/O loops that yield frequently — but it
is a real semantic shift. If an application registers a future that does
heavy synchronous work between awaits, that work now blocks every other
AimDB future, not just itself. Document this in the user-facing migration
note. (The eventual lift of `std` gating on AimX, when it lands via the
follow-up issue, will further amplify this — at that point the AimX
supervisor lives in the same shared stack.)

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

### Other affected crates

Beyond the connector trait, three additional crates carry `R: Spawn` bounds
that must be loosened to `R: RuntimeAdapter` (mechanical, no behaviour change):

- **`aimdb-codegen`** ([rust.rs:560,734,1253](../../aimdb-codegen/src/rust.rs#L560)) — the schema-codegen emits `use aimdb_executor::Spawn;` and `<R: Spawn + 'static>` into generated `configure_schema` functions. Update the emitter and the two golden tests at [rust.rs:1836,1941](../../aimdb-codegen/src/rust.rs#L1836).
- **`aimdb-persistence`** ([builder_ext.rs:23](../../aimdb-persistence/src/builder_ext.rs#L23), [ext.rs:19,32](../../aimdb-persistence/src/ext.rs#L19), [query_ext.rs:59,68](../../aimdb-persistence/src/query_ext.rs#L59)) — five `R: Spawn` bounds across three files.
- **`aimdb-sync`** ([handle.rs](../../aimdb-sync/src/handle.rs)) — sync-API handle types carry `R: Spawn`.

None of these crates *call* `runtime.spawn`; they only propagate the bound.
Removing the bound is a find-replace.

---

## Breaking Changes

| Area | Change |
|---|---|
| Public API | `db.run().await` must be called after `build()` — tasks do not start until `run()` |
| `Runtime` supertrait | `Spawn` removed — custom adapters no longer need to implement it |
| `Spawn` trait | Deleted entirely from `aimdb-executor` (`SpawnToken` associated type goes with it) |
| `ExecutorError::SpawnFailed` | Variant removed |
| `AimDb::spawn_task` | Public method **removed** — use `on_start()` or nested `FuturesUnordered` |
| `EmbassyAdapter::new_with_spawner` | Constructor removed |
| `EmbassyAdapter::new_with_network(spawner, network)` | Signature changes to `(network)` — `spawner` arg removed |
| `EmbassyAdapter` feature flags | `embassy-task-pool-8/16/32` removed |
| `ConnectorBuilder` trait | `build()` now returns `Vec<BoxFuture>` instead of `Box<dyn Connector>` |
| `TokioAdapter::spawn_connectors` | Removed (test-only helper, unused) |
| Generated code (codegen) | `configure_schema<R: Spawn + 'static>` → `<R: RuntimeAdapter + 'static>` — regenerate downstream |
| `aimdb-persistence`, `aimdb-sync` trait bounds | `R: Spawn` → `R: RuntimeAdapter` on public traits |
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

### Step 3a — Core: hoist join-transform forwarder spawning

**File:** `aimdb-core/src/transform/join.rs`

`run_join_transform` ([join.rs:329](../../aimdb-core/src/transform/join.rs#L329))
currently calls `runtime.spawn(forwarder_future)` lazily inside the
already-running transform task. The forwarder count equals `inputs.len()`,
which is known when the `JoinPipeline` is registered.

- Change `JoinPipeline::into_descriptor()` to return both the transform
  future *and* the forwarder futures: e.g.
  `TransformDescriptor { task_future, fanin_futures: Vec<BoxFuture<'static, ()>> }`.
- Construct the `JoinTrigger` queue and forwarder futures at descriptor
  construction time (build phase), not inside `run_join_transform`.
- `collect_transform_future` (Step 3) appends both `task_future` and every
  `fanin_future` to the accumulator vec.
- Delete the `runtime.spawn(...)` call inside `run_join_transform`.

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
  `AimDbInner::get_typed_record_by_key/id`, **and the manual
  `impl Clone for AimDb<R>` at [builder.rs:1037](../../aimdb-core/src/builder.rs#L1037)**.
- **Delete `AimDb::spawn_task`** ([builder.rs:1096](../../aimdb-core/src/builder.rs#L1096)).
  No internal callers; downstream callers migrate to `on_start()` or nested
  `FuturesUnordered`.
- Collapse the `std`/`no_std` `on_start` bifurcation: the two type aliases
  at [builder.rs:32-47](../../aimdb-core/src/builder.rs#L32-L47) are **already
  identical** — the bifurcation is vestigial. Unify into a single
  `type StartFnType<R> = Box<dyn FnOnce(Arc<R>) -> BoxFuture<'static, ()> + Send>;`
  and remove the `#[cfg(feature = "std")]` split. No closure-bound change
  needed.

---

### Step 5 — Core: remote access bounds

**Files:** `aimdb-core/src/remote/supervisor.rs`,
`aimdb-core/src/remote/handler.rs`, `aimdb-core/src/builder.rs`

- Rename `spawn_supervisor` → `build_supervisor_future`; return
  `DbResult<BoxFuture<'static, ()>>` instead of spawning.
- Remove `R: Spawn` bounds from all 14 handler function signatures; replace
  with `R: RuntimeAdapter`.
- `build()` in `builder.rs` calls `build_supervisor_future()` and pushes the
  returned future to the accumulator.
- **`AimDb::subscribe_record_updates`** ([builder.rs:1409](../../aimdb-core/src/builder.rs#L1409))
  currently calls `runtime.spawn(...)`. Rewrite to call `tokio::spawn`
  directly under `#[cfg(feature = "std")]`. This is the bridge state — the
  full nested-`FuturesUnordered` rewrite is deferred to the follow-up issue.
- Bare `tokio::spawn` calls at `supervisor.rs:122` and `handler.rs:1042`
  remain **unchanged** in this PR; they are addressed in the follow-up.

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

### Step 9a — Codegen

**File:** `aimdb-codegen/src/rust.rs`

- Replace emitted `use aimdb_executor::Spawn;` with whatever the new bundle
  exports (or drop it — `AimDbBuilder<R>` no longer needs an explicit bound).
- Change emitted `<R: Spawn + 'static>` to `<R: aimdb_executor::RuntimeAdapter + 'static>` in `configure_schema` signatures (lines 560, 734, 1253).
- Update the two golden-string tests at lines 1836 and 1941.

### Step 9b — Persistence and sync

- **`aimdb-persistence`** (`builder_ext.rs`, `ext.rs`, `query_ext.rs`):
  replace every `R: Spawn` with `R: RuntimeAdapter`. No runtime calls
  change.
- **`aimdb-sync`** (`handle.rs`): same find-replace.

### Step 9c — Delete `TokioAdapter::spawn_connectors`

`aimdb-tokio-adapter/src/connector.rs`: delete the helper and its tests.
Test-only, no external callers.

---

### Step 10 — Examples and aimdb-pro call sites

Update all examples to add `runner.run().await` after `build()`. Update
aimdb-pro demo binaries, connectors, and any internal tooling that calls
`build()`. Three Embassy examples plus aimdb-pro docs require an
`EmbassyAdapter::new_with_network(spawner, stack)` → `new_with_network(stack)`
edit:

- `examples/embassy-knx-connector-demo/src/main.rs`
- `examples/embassy-mqtt-connector-demo/src/main.rs`
- `examples/weather-mesh-demo/weather-station-gamma/src/main.rs`
- aimdb-pro UI docs (`llms-full.txt`, `04-deployment.md`)

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

### E — Channel-based `AimDbSpawner` handle

A handle returned from `build()` that can submit new futures into the
running `FuturesUnordered` via an unbounded mpsc. **Rejected** — adds API
surface (`AimDbSpawner`, channel polling in `run()`) for a use case (AimX
per-connection / per-subscription fan-out) that has a strictly cleaner
local solution: nested `FuturesUnordered` inside the supervisor future
itself. See [Out of Scope](#out-of-scope).

### F — Nested `FuturesUnordered` for AimX, inside this PR

Combine the AimX portability refactor with the trait removal. **Deferred** —
two unrelated changes, doubles the review surface, and the AimX rewrite
touches subscription cancellation semantics (today: `oneshot::Sender` per
subscription; after: drop the future). Cleaner as a focused follow-up.
See [Out of Scope](#out-of-scope).

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
   Unify into a single `StartFnType<R>` alias (see Step 4 for details). The
   two existing aliases are already byte-for-byte identical; this is a pure
   simplification.

7. **Join-transform forwarders** → **Hoist to build time.**
   `inputs.len()` is statically known when `transform_join()` registers the
   pipeline. The lazy `runtime.spawn(forwarder)` inside `run_join_transform`
   becomes build-time collection (Step 3a). This keeps the "no spawn
   reachable from `aimdb-core` after `build()`" invariant clean — modulo
   the AimX exception below.

8. **AimX remote-access portability** → **Defer to follow-up issue.**
   The supervisor and handler currently call bare `tokio::spawn` for
   per-connection and per-subscription tasks. These do **not** depend on
   the `Spawn` trait, so they can stay as-is for this PR (AimX is
   `std`-gated). A separate issue replaces them with nested
   `FuturesUnordered` driven by `select_biased!`, which makes the AimX
   path runtime-agnostic and is the prerequisite for eventually un-gating
   AimX from `std`. See [Out of Scope](#out-of-scope).

9. **`AimDb::spawn_task`** → **Delete.**
   Public convenience method with no internal callers. After the trait
   removal there is no portable backing primitive. Downstream callers
   migrate to `on_start()` (build-time collection) or to a private
   `FuturesUnordered` inside their own future. No deprecation cycle;
   already breaking release.

---

## Out of Scope

The following are explicitly **not** part of this PR / issue #88:

### ~~AimX remote-access spawn-free refactor~~ (resolved by design 030)

Originally deferred to a follow-up; landed via
[design 030](030-M13-aimx-remote-spawn-free.md) /
[issue #114](https://github.com/aimdb-dev/aimdb/issues/114). All three
bridge-state `tokio::spawn` sites in `aimdb-core/src/remote/` were
replaced with nested `FuturesUnordered`; `subscribe_record_updates` was
deleted in favour of a `Stream`-returning helper; per-subscription
`oneshot` cancel channels were replaced with `Arc<Notify>` notifies for immediate unsubscribe.

### ~~WebSocket client reconnect spawn~~ (resolved by design 030)

Originally deferred alongside the AimX follow-up; resolved in the same
PR. The six `tokio::spawn` sites in
[`aimdb-websocket-connector/src/client/connector.rs`](../../aimdb-websocket-connector/src/client/connector.rs)
collapsed into one connector future that owns a `FuturesUnordered`; the
reconnect watcher sends `NewLoops` over an mpsc rather than spawning.

### Removing `R` from `Producer<T, R>` and `Consumer<T, R>`

Already deferred (Alternative C). Touches every public API signature and is
a larger surface change with no functional benefit on top of this refactor.
