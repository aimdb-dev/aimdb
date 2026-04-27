# `no_std` Support for the Transform API

**Version:** 1.1
**Status:** Draft
**Last Updated:** 2026-04-26
**Issue:** [#73 — `no_std` Support for Transform API](https://github.com/schnorr-lab/aimdb/issues/73)
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
- [Acceptance Criteria](#acceptance-criteria)

---

## Summary

The transform API (`TransformBuilder`, `StatefulTransformBuilder`, `TransformPipeline`) is
partially usable in `no_std` environments today, but the multi-input join path
(`JoinBuilder`, `JoinStateBuilder`, `JoinPipeline`, `run_join_transform`) is currently
`std`-only because join fan-in is hardcoded to `tokio::sync::mpsc`.

This revision adopts a clean end-state architecture: join fan-in is defined in
`aimdb-executor` as runtime capabilities, and implemented by each runtime adapter
(Tokio, Embassy, WASM, future runtimes). `aimdb-core` no longer imports Tokio- or
Embassy-specific queue types for join execution.

This intentionally allows API changes across crates to achieve a single, runtime-agnostic
join implementation.

---

## Current State

### Symbols that are `std`-only and why

| Symbol | Location | Root dependency |
|---|---|---|
| `JoinBuilder<O, R>` | `transform.rs` | `tokio::sync::mpsc::UnboundedSender` in `JoinInputFactory` |
| `JoinInputFactory<R>` | `transform.rs` | `tokio::sync::mpsc::UnboundedSender` |
| `JoinStateBuilder<O, S, R>` | `transform.rs` | Propagated from `JoinBuilder` |
| `JoinPipeline<O, R>` | `transform.rs` | Propagated from `JoinBuilder` |
| `run_join_transform(...)` | `transform.rs` | `tokio::sync::mpsc::unbounded_channel()` |
| `transform_join_raw()` | `typed_api.rs` | `JoinBuilder` / `JoinPipeline` |
| `transform_join()` in `impl_record_registrar_ext!` | `ext_macros.rs` | Same |

### What already works in `no_std`

- `TransformDescriptor` is alloc-only.
- `TransformBuilder` / `StatefulTransformBuilder` / `TransformPipeline` have no std dependency.
- `run_single_transform` is async and runtime-agnostic.
- `TypedRecord::set_transform()` already works on `no_std + alloc`.

### What is broken for `no_std`

- Multi-input join is unavailable because fan-in is not abstracted by runtime traits.
- API exposure (`transform_join`) follows that same std-only wiring.

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

```rust
registrar
    .register::<HeatIndex>("sensor::HeatIndex", buffer_sized::<4, 2>(SpmcRing))
    .transform_join(|b| {
        b.input::<Temperature>("sensor::Temperature")
         .input::<Humidity>("sensor::Humidity")
         .with_state(HeatIndexState::default())
         .on_trigger(|trigger, state, producer| async move {
             match trigger.index() {
                 0 => state.temperature = trigger.as_input::<Temperature>().copied(),
                 1 => state.humidity = trigger.as_input::<Humidity>().copied(),
                 _ => {}
             }
             if let (Some(t), Some(h)) = (state.temperature, state.humidity) {
                 let _ = producer.produce(heat_index(t, h)).await;
             }
         })
    });
```

Queue capacity is an internal runtime detail and is not part of the user-facing API.
Each runtime adapter documents its fixed default.

---

## Implementation Checklist

### Step 1 — Core unblocking
- [ ] Ensure `.transform()` compiles with embedded target (`thumbv7em-none-eabihf`) under `alloc`.
- [ ] Gate join-only exports/features from single-input path where needed.

### Step 2 — Executor contract
- [ ] Add `join` module and fan-in traits to `aimdb-executor`.
- [ ] Add tests for trait behavior contract (bounded semantics, send/recv errors).
- [ ] Re-export new traits from `aimdb-executor` root.

### Step 3 — Core refactor
- [ ] Remove direct `tokio::mpsc` usage from `aimdb-core` join pipeline.
- [ ] Refactor `JoinBuilder`/`JoinPipeline` to depend on executor join traits.
- [ ] Update `typed_api.rs` and extension macros to call unified join API.

### Step 4 — Runtime adapter implementations
- [ ] Implement join fan-in traits in `aimdb-tokio-adapter`.
- [ ] Implement join fan-in traits in `aimdb-embassy-adapter`.
- [ ] Implement join fan-in traits in `aimdb-wasm-adapter`.
- [ ] Enable any required optional dependencies and feature wiring.

### Step 5 — Validation
- [ ] `cargo check --target thumbv7em-none-eabihf --features embassy-runtime` passes for `.transform()`.
- [ ] `cargo check --target thumbv7em-none-eabihf --features embassy-runtime` passes for `.transform_join()`.
- [ ] `cargo check --target wasm32-unknown-unknown -p aimdb-wasm-adapter` passes for join fan-in implementation.
- [ ] Workspace tests pass on std path.
- [ ] Add integration tests showing identical join behavior on Tokio, Embassy, and WASM adapters.

### Docs
- [ ] Update transform docs to describe runtime-owned fan-in model.
- [ ] Document each adapter's fixed queue capacity and backpressure behaviour.
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

## Open Questions & Required Clarifications

The following gaps were identified during codebase review. Each needs an explicit decision
before the corresponding implementation step begins.

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

**Decision needed**: Confirm the proposed per-adapter defaults (64 / 8 / 64), or nominate
different values.

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

**Decision needed**: None. Confirm `futures::channel::mpsc` is the preferred queue primitive
for WASM, or nominate an alternative (e.g., `async-channel` which is `no_std + alloc` friendly).

---

## Acceptance Criteria

1. `aimdb-core` join code contains no Tokio- or Embassy-specific imports.
2. `aimdb-executor` exposes join fan-in runtime traits used by `aimdb-core`.
3. Tokio, Embassy, and WASM adapters implement join fan-in traits.
4. Embedded target build passes with both single-input and multi-input transform usage.
5. Runtime-specific queue types are not exposed in public transform API.
6. WASM target build (`wasm32-unknown-unknown`) passes with join fan-in enabled.
7. Join behavior is bounded and documented consistently across runtimes.
