# `no_std` Support for the Transform API

**Version:** 1.3
**Status:** Implemented
**Last Updated:** 2026-05-03
**Issue:** [#73 — `no_std` Support for Transform API](https://github.com/aimdb-dev/aimdb/issues/73)
**Milestone:** M10 / M11 — Embedded First-Class Support
**Depends On:** [020-M9-transform-api](020-M9-transform-api.md)

---

## Table of Contents

- [Summary](#summary)
- [Current State](#current-state)
- [Goals and Non-Goals](#goals-and-non-goals)
- [Design](#design)
  - [Step 1 — Unblock single-input transform on `no_std + alloc`](#step-1--unblock-single-input-transform-on-no_std--alloc)
  - [Step 2 — Move join fan-in into `aimdb-executor`](#step-2--move-join-fan-in-into-aimdb-executor)
  - [Step 3 — Refactor `aimdb-core` join pipeline to runtime traits](#step-3--refactor-aimdb-core-join-pipeline-to-runtime-traits)
    - [Step 4 — Implement fan-in in Tokio, Embassy, and WASM adapters](#step-4--implement-fan-in-in-tokio-embassy-and-wasm-adapters)
- [File Structure After Refactor](#file-structure-after-refactor)
- [API Surface](#api-surface)
- [Implementation Checklist](#implementation-checklist)
- [Alternatives Considered](#alternatives-considered)
- [Risk & Constraints](#risk--constraints)
- [Open Questions & Required Clarifications](#open-questions--required-clarifications)
  - [Q4 — `on_trigger` callback model prevents full async in the handler](#q4--on_trigger-callback-model-prevents-full-async-in-the-handler)
- [Acceptance Criteria](#acceptance-criteria)

---

## Summary

This revision made the full transform API — both single-input and multi-input join —
available on `no_std + alloc` targets. Previously the join path was `std`-only because
fan-in was hardcoded to `tokio::sync::mpsc`.

The adopted architecture: join fan-in is defined in `aimdb-executor` as runtime-agnostic
traits (`JoinFanInRuntime`, `JoinQueue`, `JoinSender`, `JoinReceiver`) and implemented by
each runtime adapter (Tokio, Embassy, WASM). `aimdb-core` contains no Tokio- or
Embassy-specific types in the join path.

During implementation, the join handler API was also redesigned: the original callback
model (`with_state().on_trigger(Fn(...) -> Pin<Box<dyn Future>>)`) was replaced with a
task model (`on_triggers(FnOnce(JoinEventRx, Producer) -> impl Future)`). This eliminated
per-event heap allocation and the restriction that state could not be borrowed across
`.await` points. See [Q4](#q4--on_trigger-callback-model-prevents-full-async-in-the-handler)
and [Alternative D](#alternative-d--keep-the-callback-model-on_trigger) for the full
trade-off record.

---

## State Before This Revision

### Symbols that were `std`-only and why

| Symbol | Location | Root dependency |
|---|---|---|
| `JoinBuilder<O, R>` | `transform.rs` | `tokio::sync::mpsc::UnboundedSender` in `JoinInputFactory` |
| `JoinInputFactory<R>` | `transform.rs` | `tokio::sync::mpsc::UnboundedSender` |
| `JoinStateBuilder<O, S, R>` | `transform.rs` | Propagated from `JoinBuilder` |
| `JoinPipeline<O, R>` | `transform.rs` | Propagated from `JoinBuilder` |
| `run_join_transform(...)` | `transform.rs` | `tokio::sync::mpsc::unbounded_channel()` |
| `transform_join_raw()` | `typed_api.rs` | `JoinBuilder` / `JoinPipeline` |
| `transform_join()` in `impl_record_registrar_ext!` | `ext_macros.rs` | Same |

### What already worked in `no_std`

- `TransformDescriptor` was alloc-only.
- `TransformBuilder` / `StatefulTransformBuilder` / `TransformPipeline` had no std dependency.
- `run_single_transform` was async and runtime-agnostic.
- `TypedRecord::set_transform()` already worked on `no_std + alloc`.

### What was broken for `no_std`

- Multi-input join was unavailable because fan-in was not abstracted by runtime traits.
- API exposure (`transform_join`) followed that same std-only wiring.

---

## Goals and Non-Goals

**Goals:**
- Make single-input `.transform()` available on `no_std + alloc`.
- Make multi-input `.transform_join()` available on `no_std + alloc` without embedding Embassy or Tokio types in `aimdb-core`.
- Define join fan-in once in `aimdb-executor`, implemented by runtime adapters.
- Use one join engine in `aimdb-core` for all runtimes.
- Standardize backpressure semantics for join fan-in (bounded queue).
- Add a WASM join fan-in implementation now (same core join engine, no core fork).

**Non-goals:**
- Supporting `no_std` without `alloc`.
- Preserving source compatibility for current `JoinBuilder` internals.
- Adding runtime-specific APIs to user-facing transform signatures.

---

## Design

### Step 1 — Unblock single-input transform on `no_std + alloc`

Make sure single-input transform path compiles independently from join internals.

Concrete changes:

1. Gate join-only public re-exports (`JoinBuilder`, `JoinPipeline`, `JoinTrigger`) behind
   `#[cfg(feature = "alloc")]`; the `#[cfg(feature = "std")]` gate is removed in Step 3 (see Q2).
2. Verify `.transform()` compiles on embedded target before join refactor lands.

---

### Step 2 — Move join fan-in into `aimdb-executor`

Introduce runtime contracts for join fan-in queue creation and usage.

Proposed trait shape (conceptual):

```rust
pub trait JoinFanInRuntime: Spawn {
    type JoinQueue<T: Send + 'static>: JoinQueue<T>;

    fn create_join_queue<T: Send + 'static>(&self) -> DbResult<Self::JoinQueue<T>>;
}

pub trait JoinQueue<T: Send + 'static> {
    type Sender: JoinSender<T> + Clone + Send + 'static;
    type Receiver: JoinReceiver<T> + Send + 'static;

    fn split(self) -> (Self::Sender, Self::Receiver);
}

pub trait JoinSender<T: Send + 'static> {
    async fn send(&self, item: T) -> DbResult<()>;
}

pub trait JoinReceiver<T: Send + 'static> {
    async fn recv(&mut self) -> DbResult<T>;
}
```

Key decisions:

- Queue capacity is an internal runtime detail — not exposed in the trait or user-facing API.
- Each runtime adapter chooses a fixed, documented default (e.g., Tokio: 64, Embassy: 8).
- Backpressure is explicit and consistent: `send().await` may await when full.
- Runtime owns queue construction details (`tokio::mpsc`, `embassy_sync::Channel`, etc.).

---

### Step 3 — Refactor `aimdb-core` join pipeline to runtime traits

Rework join pipeline internals to depend only on executor traits.

Key changes:

1. `JoinBuilder` stores join input metadata only, not concrete sender types or capacity.
2. `run_join_transform` requests queue from runtime via `JoinFanInRuntime::create_join_queue`.
3. Forwarder tasks use generic `JoinSender`.
4. Trigger loop uses generic `JoinReceiver`.
5. Remove Tokio imports from join path in `aimdb-core`.

This yields one join implementation shared by std and no_std runtimes.

---

### Step 4 — Implement fan-in in Tokio, Embassy, and WASM adapters

Runtime adapters provide concrete queue implementations.

Tokio adapter:

- Implement queue using bounded `tokio::sync::mpsc::channel` with a fixed internal default capacity.
- Map send/recv closure semantics into `DbResult`.

Embassy adapter:

- Implement queue using `embassy_sync::channel::Channel`.
- Allocate queue storage through adapter-owned resources for DB lifetime
  (for example `Box::leak(Box::new(Channel::new()))` under alloc).
- Preserve no_std compatibility and avoid std imports.

WASM adapter:

- Implement queue using a WASM-safe async queue primitive (for example `futures::channel::mpsc`).
- Keep behavior consistent with the fan-in contract (bounded semantics, ordered delivery).
- Ensure it works on `wasm32-unknown-unknown` without introducing host-only dependencies.

---

## File Structure After Refactor

```
aimdb-executor/src/
    join.rs                # Join fan-in traits (runtime contract)
    lib.rs                 # Re-export join traits

aimdb-core/src/
    transform/
        mod.rs             # shared transform API and exports
        single.rs          # single-input transform path
        join.rs            # runtime-agnostic join implementation

aimdb-tokio-adapter/src/
    join_queue.rs          # tokio JoinFanInRuntime implementation

aimdb-embassy-adapter/src/
    join_queue.rs          # embassy JoinFanInRuntime implementation

aimdb-wasm-adapter/src/
    join_queue.rs          # wasm JoinFanInRuntime implementation
```

No split between `join_std.rs` and `join_nostd.rs` in core.

---

## API Surface

### Single-input

No intentional user-facing changes.

### Multi-input join

User-facing API is unified across runtimes and does not require runtime queue types.
`JoinStateBuilder` and `with_state().on_trigger()` were replaced by `on_triggers()` —
see [Q4](#q4--on_trigger-callback-model-prevents-full-async-in-the-handler).

```rust
registrar
    .configure::<HeatIndex>("sensor::HeatIndex", |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SpmcRing)
           .transform_join(|b| {
               b.input::<Temperature>("sensor::Temperature")
                .input::<Humidity>("sensor::Humidity")
                .on_triggers(|mut rx, producer| async move {
                    let mut last_t: Option<Temperature> = None;
                    let mut last_h: Option<Humidity> = None;
                    while let Ok(trigger) = rx.recv().await {
                        match trigger.index() {
                            0 => last_t = trigger.as_input::<Temperature>().cloned(),
                            1 => last_h = trigger.as_input::<Humidity>().cloned(),
                            _ => {}
                        }
                        // Borrow both across .await — possible because both are
                        // owned by this async block, not borrowed from a caller frame.
                        if let (Some(t), Some(h)) = (&last_t, &last_h) {
                            producer.produce(heat_index(t, h)).await.ok();
                        }
                    }
                })
           });
    });
```

`JoinEventRx` is the type of `rx` — a type-erased wrapper around the runtime's concrete
receiver, allocated once at task startup. Its `recv().await` returns `Ok(JoinTrigger)`
until all upstream input forwarders have exited.

Queue capacity is an internal runtime detail and is not part of the user-facing API.
Each runtime adapter documents its fixed default (Tokio: 64, Embassy: 8, WASM: 64).

---

## Implementation Checklist

### Step 1 — Core unblocking
- [x] Ensure `.transform()` compiles with embedded target (`thumbv7em-none-eabihf`) under `alloc`.
- [x] Gate join-only exports/features from single-input path where needed.

### Step 2 — Executor contract
- [x] Add `join` module and fan-in traits to `aimdb-executor`.
- [x] Add tests for trait behavior contract (bounded semantics, send/recv errors).
- [x] Re-export new traits from `aimdb-executor` root.

### Step 3 — Core refactor
- [x] Remove direct `tokio::mpsc` usage from `aimdb-core` join pipeline.
- [x] Refactor `JoinBuilder`/`JoinPipeline` to depend on executor join traits. `JoinStateBuilder` removed; `on_triggers(FnOnce)` task model adopted (see Q4).
- [x] Update `typed_api.rs` and extension macros to call unified join API.

### Step 4 — Runtime adapter implementations
- [x] Implement join fan-in traits in `aimdb-tokio-adapter`.
- [x] Implement join fan-in traits in `aimdb-embassy-adapter`. Gated on `#[cfg(all(feature = "embassy-runtime", feature = "alloc"))]`.
- [x] Implement join fan-in traits in `aimdb-wasm-adapter`.
- [x] Enable any required optional dependencies and feature wiring.

### Step 5 — Validation
- [x] `cargo check --target thumbv7em-none-eabihf --features embassy-runtime` passes for `.transform()`.
- [x] `cargo check --target thumbv7em-none-eabihf --features embassy-runtime` passes for `.transform_join()`. Embassy target is ARM-only compile-checked; full execution tests run on Tokio/WASM.
- [x] `cargo check --target wasm32-unknown-unknown -p aimdb-wasm-adapter` passes for join fan-in implementation.
- [x] Workspace tests pass on std path (`make check`, `make all`).
- [x] Integration tests added for Tokio and WASM adapters showing `transform_join` with inline `on_triggers` closure.

### Docs
- [x] API Surface updated with `on_triggers` task-model example (this document).
- [x] Each adapter's fixed queue capacity documented (Q1 resolution below).
- [ ] Update `020-M9-transform-api.md` with new join API notes.

---

## Alternatives Considered

### Alternative A — Keep split `join_std.rs` / `join_nostd.rs`

Simple to implement, but duplicates join logic and keeps runtime details inside core.

**Decision:** Rejected.

### Alternative B — Keep Tokio + Embassy only for now

Defers browser/WASM parity and creates another follow-up migration.

**Decision:** Rejected.

### Alternative C — Cross-runtime queue abstraction in `aimdb-core` only

Avoids touching executor crate but still couples core to runtime concerns and grows
internal complexity.

**Decision:** Rejected in favor of executor-owned contract.

### Alternative D — Keep the callback model (`on_trigger`)

The original API used a per-event callback:

```rust
.with_state(initial_state)
.on_trigger(|trigger, state, producer| {
    // must return Pin<Box<dyn Future + Send + 'static>>
})
```

The framework owned the event loop and called the user closure once per incoming trigger.
This avoided user-visible `while` loops and felt similar to stream combinators.

The cost: `Fut: 'static` forced every reference to `state` or `producer` to be cloned or
copied synchronously before the first `.await`. Any async work in the handler body needed a
`Pin<Box<...>>` allocation on every trigger even for no-op branches. Named function syntax
was required because async closures do not compose with HRTB in stable Rust.

**Decision:** Rejected in favor of the task model (`on_triggers`). The task model costs one
`Box` at task startup instead of one per event, allows state to be borrowed freely across
`.await` points (owned by the `async move` block), allows inline closure syntax, and makes
the event loop explicit — which is also more composable (users can `select!` with other
futures or break early).

---

## Risk & Constraints

| Risk | Mitigation |
|---|---|
| Cross-crate refactor complexity | Land in staged PRs: executor traits, core refactor, adapters, then API/docs |
| Semantic shift from unbounded std queue to bounded queue | Explicitly document behavior change; each adapter publishes its fixed default capacity |
| Embassy queue lifetime/storage management | Keep storage ownership in adapter and test long-running workload behavior |
| WASM single-threaded execution model differences | Keep fan-in contract executor-agnostic and test queue behavior under wasm target |
| Trait async ergonomics across no_std/std | Use existing async trait patterns already accepted in executor crate |
| Breakage in extension macros and typed API wiring | Add compile-only tests for macro expansions on both runtime feature sets |

---

## Open Questions & Resolutions

The following gaps were identified during codebase review. All are now resolved.

---

### Q1 — Queue capacity: remove from trait and user API entirely

**Problem**

`embassy_sync::channel::Channel<M, T, N>` requires `N` as a compile-time const generic.
A `create_join_queue(capacity: usize)` signature implies runtime-selected capacity, which
Embassy structurally cannot honour. The same constraint is already documented in the Embassy
buffer implementation (`buffer.rs` panics if a runtime capacity doesn't match `CAP`).

A user-facing `.capacity()` override on `JoinBuilder` has the same issue on Embassy, and adds
API surface with limited practical value on any runtime — the join queue is an internal buffer
between forwarder tasks and the trigger loop, not something most users need to tune.

**Affected steps**: Step 2 (trait shape) and Step 3 (core refactor)

**Resolution (already applied to this document)**

`create_join_queue` takes no capacity argument. Queue capacity is an internal constant chosen
by each adapter and documented in its crate:

- Tokio adapter: `64` (bounded `tokio::sync::mpsc::channel`)
- Embassy adapter: `8` (compile-time const, `embassy_sync::channel::Channel`)
- WASM adapter: `64` (bounded `futures_channel::mpsc::channel`)

The `.capacity()` method is removed from `JoinBuilder`. No user-facing knob exists on any
runtime. If a specific workload needs a different size, the adapter constant can be raised in
a future release.

**Resolved**: Per-adapter defaults confirmed and implemented — Tokio: 64, Embassy: 8, WASM: 64.
No user-facing capacity knob exists on any runtime.

---

### Q2 — `#[cfg(feature = "std")]` gate on join types is no longer correct after the refactor

**Problem**

Every `#[cfg(feature = "std")]` on a join symbol (`JoinBuilder`, `JoinStateBuilder`,
`JoinPipeline`, `run_join_transform`, `transform_join_raw`, `transform_join` in the macro)
exists for one reason: `tokio::sync::mpsc::UnboundedSender` in `JoinInputFactory`. After
Step 3, that type is gone.

The join types then only need:

1. `alloc` — for `Box`, `Vec`, `String` (already required by the rest of `aimdb-core` on
   embedded targets).
2. `R: JoinFanInRuntime` — a trait bound, enforced by the type system, not a cfg flag.

There is no `std` dependency remaining. Keeping the `std` gate would mean Embassy and WASM
builds can never use `transform_join`, which is the opposite of the goal.

The secondary issue from the previous version of this question — the multi-feature macro arm
(Embassy) has no `transform_join` declaration at all — is a symptom of the same root cause:
the method was only ever added under `#[cfg(feature = "std")]`.

**Affected steps**: Step 3 (core refactor — all gates change) and Step 4 (macro extension)

**Resolution**

Replace `#[cfg(feature = "std")]` with `#[cfg(feature = "alloc")]` on all join types and
functions in `transform.rs` and `typed_api.rs`. Remove the `std` gate from `transform_join`
in the macro entirely — the `R: JoinFanInRuntime` where clause is the correct compile-time
gate:

```rust
// ext_macros.rs — both single-feature and multi-feature arms
fn transform_join<F>(
    &'a mut self,
    build_fn: F,
) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
where
    $runtime: aimdb_executor::JoinFanInRuntime,
    F: FnOnce(
        $crate::transform::JoinBuilder<T, $runtime>,
    ) -> $crate::transform::JoinPipeline<T, $runtime>;
```

If a runtime does not implement `JoinFanInRuntime`, the compiler will reject `.transform_join()`
calls with a clear "trait not satisfied" error — no cfg flag needed.

The `lib.rs` re-exports follow the same change:

```rust
// Before (wrong after refactor)
#[cfg(feature = "std")]
pub use transform::{JoinBuilder, JoinPipeline, JoinTrigger};

// After
#[cfg(feature = "alloc")]
pub use transform::{JoinBuilder, JoinPipeline, JoinTrigger};
```

No new `join-runtime` feature flag is needed.

**Staging note**: the macro extension (adding `transform_join` to the multi-feature arm) and
the Embassy `JoinFanInRuntime` impl must land in the same PR. If the method is added to the
trait without the adapter impl, the `impl EmbassyRecordRegistrarExt for RecordRegistrar<...,
EmbassyAdapter>` block will fail to satisfy the `JoinFanInRuntime` bound.

**Resolved**: All `#[cfg(feature = "std")]` gates on join types replaced with
`#[cfg(feature = "alloc")]` in `transform.rs`, `typed_api.rs`, and `lib.rs`. The macro
extension was updated in both the single-feature and multi-feature arms. Embassy adapter
`JoinFanInRuntime` impl gated on `#[cfg(all(feature = "embassy-runtime", feature = "alloc"))]`
and landed together with the macro change.

---

### Q3 — `aimdb-wasm-adapter` is missing `futures-channel` for the WASM fan-in

**Problem**

The design proposes `futures::channel::mpsc` for the WASM fan-in queue. The current
`aimdb-wasm-adapter/Cargo.toml` only lists `futures-util` (alloc-only) — `futures-channel`
is a separate sub-crate and is absent.

**Affected step**: Step 4 (WASM fan-in implementation)

**Proposed resolution**

Add to `aimdb-wasm-adapter/Cargo.toml`:

```toml
futures-channel = { version = "0.3", default-features = false, features = ["alloc"] }
```

This is a one-line Cargo.toml change. No design decision needed; listing it here so it is not
forgotten when the WASM fan-in PR is opened.

**Resolved**: `futures-channel = { version = "0.3", features = ["std", "sink"] }` added to
`aimdb-wasm-adapter/Cargo.toml`. Note: `features = ["alloc"]` alone is insufficient —
`BiLock` inside `futures-channel` depends on `std::sync`; `std` is the minimum required
feature. `futures::channel::mpsc` confirmed as the WASM fan-in primitive.

---

### Q4 — `on_trigger` callback model prevents full async in the handler

**Problem**

The current `on_trigger` signature:

```rust
F: Fn(JoinTrigger, &mut S, &Producer<O, R>) -> Fut + Send + Sync + 'static,
Fut: core::future::Future<Output = ()> + Send + 'static,
```

requires `Fut: 'static`, but `state` and `producer` are borrowed from the framework's event
loop stack frame — not `'static`. Any future that references them would inherit their lifetime,
violating the bound. The consequence is that the handler body cannot borrow `state` or
`producer` across an `.await` point. All values needed in the async portion must be cloned or
copied out synchronously before the first `.await`, and every trigger — including no-op
branches — requires a `Box::pin()` heap allocation.

**Root cause**

The framework owns the event loop and calls user code as a *callback per event* (`Fn`). The
canonical way to express "this future borrows from its arguments" would be higher-ranked trait
bounds (`for<'a> Fn(&'a mut S) -> Fut<'a>`), but async closures do not compose cleanly with
HRTB in stable Rust. The API escapes the problem by requiring `Fut: 'static` and shifting the
burden onto the caller.

The current workaround (synchronous state update → clone into `async move` → `Box::pin`) is
functional but non-obvious, requires a named function instead of an inline closure, and
allocates on every event.

**Proposed alternative — task model**

Change the ownership model: instead of the framework running `while let Ok(trigger) = rx.recv().await`
and calling a user callback on each iteration, hand the `rx` end of the fan-in queue directly
to a user-supplied `async` closure. The framework still creates the queue and spawns the
per-input forwarders; it no longer owns the event loop.

```rust
// Current — callback model (framework event loop, user callback per event)
.with_state(initial_state)
.on_trigger(|trigger, state, producer| {
    // must return Pin<Box<dyn Future + Send + 'static>>
    // cannot borrow state or producer across .await
})

// Proposed — task model (user owns the event loop and state)
.on_triggers(|mut rx, producer| async move {
    let mut state = initial_state;
    while let Ok(trigger) = rx.recv().await {
        // state is owned — can borrow across .await freely
        // producer is owned — no clone needed
        match trigger.index() { ... }
        if ready {
            let result = some_async_computation(&state.field).await;
            producer.produce(result).await;
        }
    }
})
```

The `rx` parameter type would be `Box<dyn JoinReceiver<JoinTrigger> + Send>` — type-erased
once at task startup (one allocation per transform, not per event) to avoid exposing the
concrete runtime queue type in the public API.

**Trade-offs**

| | Callback (`Fn`) | Task (`FnOnce`) |
|---|---|---|
| `Box::pin` allocation | Per event | Once at task start |
| Borrow state across `.await` | No | Yes |
| `with_state()` builder step | Required | Gone (state lives in closure) |
| User writes event loop | No | Yes (more explicit) |
| Full async in handler | No | Yes |
| Inline closure syntax | No (named fn required) | Yes |

The cost of the task model is that users write the `while let` loop themselves. This is more
explicit but also more flexible — users can `select!` across the trigger receiver and other
futures, implement their own timeout logic, or break early on a sentinel value.

**Affected step**: Step 3 (core refactor — `run_join_transform`, `JoinStateBuilder`,
`on_trigger` API) and Step 4 (macro/extension trait signatures).

**Resolved**: Task model adopted. `JoinStateBuilder` removed entirely. The new signature:

```rust
pub fn on_triggers<F, Fut>(self, handler: F) -> JoinPipeline<O, R>
where
    F: FnOnce(JoinEventRx, crate::Producer<O, R>) -> Fut + Send + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
```

`JoinEventRx` is a type-erased wrapper (`Box<dyn DynJoinRx + Send>`) allocated once at task
startup. Its `recv().await` is the user's event source. The framework still creates the fan-in
queue and spawns per-input forwarder tasks; user code owns the event loop. See
[Alternative D](#alternative-d--keep-the-callback-model-on_trigger) for the full trade-off
record.

---

## Acceptance Criteria

1. ✅ `aimdb-core` join code contains no Tokio- or Embassy-specific imports.
2. ✅ `aimdb-executor` exposes join fan-in runtime traits used by `aimdb-core`.
3. ✅ Tokio, Embassy, and WASM adapters implement join fan-in traits.
4. ✅ Embedded target build passes with both single-input and multi-input transform usage.
5. ✅ Runtime-specific queue types are not exposed in public transform API. `JoinEventRx` is the only type users see; its concrete queue is erased.
6. ✅ WASM target build (`wasm32-unknown-unknown`) passes with join fan-in enabled.
7. ✅ Join behavior is bounded and documented consistently across runtimes (Tokio: 64, Embassy: 8, WASM: 64).
