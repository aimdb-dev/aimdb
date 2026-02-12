# `.transform()` — Reactive Derived Records

**Version:** 1.1
**Status:** 🚧 In Progress
**Last Updated:** February 9, 2026
**Milestone:** M9 — Reactive Transforms
**Depends On:** [004-M2-api-refactor-source-tap-link](004-M2-api-refactor-source-tap-link.md), [008-M3-remote-access](008-M3-remote-access.md)
**Companion:** [021-M9-graph-introspection](021-M9-graph-introspection.md) — AimX/MCP exposure of the dependency graph

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Incident Analysis — The Bridge That Failed Silently](#incident-analysis--the-bridge-that-failed-silently)
- [Core Principles Review](#core-principles-review)
- [Problem Analysis](#problem-analysis)
- [Design Constraints](#design-constraints)
- [API Design](#api-design)
- [Architecture](#architecture)
- [Implementation](#implementation)
- [Dependency Graph](#dependency-graph)
- [AimX & MCP Exposure](#aimx--mcp-exposure) *(→ companion doc 021)*
- [Testing Strategy](#testing-strategy)
- [Implementation Plan](#implementation-plan)
- [Alternatives Considered](#alternatives-considered)
- [Future Extensions](#future-extensions)
- [Open Points & Roadblocks](#open-points--roadblocks)

---

## Summary

Add a `.transform()` method to the record registration API that declares a
**reactive, stateful derivation** from one or more input records to an output
record. This is the missing third primitive alongside `.source()` (autonomous
producer) and `.tap()` (passive observer), completing AimDB's data-flow model.

A transform:
- **Subscribes** to one or more input record buffers
- **Maintains** user-defined state across invocations
- **Produces** values into the output record's buffer
- **Is owned** by AimDB — visible in the dependency graph, introspectable via
  AimX/MCP, and lifecycle-managed by the runtime

No external channels, no shared mutexes, no `.source()` misuse.

---

## Motivation

### The Problem

AimDB's current primitives cover two extremes:

| Primitive | Role | Data flow |
|-----------|------|-----------|
| `.source()` | Autonomous producer | External world → Record |
| `.tap()` | Passive observer | Record → Side-effect |

There is no primitive for the middle ground: **Record(s) → computation → Record**.

When users need derived data today, they resort to workarounds that escape
AimDB's model:

1. **Bridge taps** — A `.tap()` that exists solely to relay data into an
   `mpsc::channel`, feeding a `.source()` on another record
2. **Shared mutable state** — `Arc<Mutex<HashMap<...>>>` shared between a
   `.tap()` on record A and a `.source()` on record B
3. **Source-that-isn't-a-source** — A `.source()` that is purely reactive
   (waits on an mpsc channel), violating its "autonomous producer" semantic

These workarounds have concrete costs:

- **Invisible dependencies** — AimDB cannot introspect the data flow from
  `Temperature` → `ForecastValidation` because the link is an mpsc channel
- **User-managed synchronization** — `Arc<Mutex<>>` is correct but boilerplate
- **Semantic confusion** — `.tap()` performing writes and `.source()` being
  reactive obscure the actual data architecture
- **No graph optimization** — AimDB cannot reorder, batch, or parallelize
  transforms it doesn't know about

### Real-World Evidence

The Weather Hub Pro demo (`aimdb-pro/demo/weather-hub-streaming`) demonstrates
every one of these workarounds in ~80 lines of bridge machinery:

```
Temperature buffer ──────────────────────────────────────────────┐
  ├── tap: ws_tap()           ← true observation ✓               │
  └── tap: relay_temps()      ← bridge tap (→ mpsc) ✗            │
                                     │                           │
ForecastBatch buffer ────────────────┼───────────────────────┐   │
  ├── tap: ws_tap()           ✓      │                       │   │
  └── tap: ingest_forecasts() ✗      │                       │   │
           │                         │                       │   │
           ▼                         ▼                       │   │
  Arc<Mutex<HashMap>>          mpsc channel                  │   │
           │                         │                       │   │
           └────────┬────────────────┘                       │   │
                    ▼                                        │   │
           validate_forecasts()  ← .source() pretending      │   │
                    │               to be reactive            │   │
                    ▼                                        │   │
           ForecastValidation buffer                         │   │
             └── tap: ws_tap() ✓                             │   │
```

**What this code wants to express:**

$$\text{ForecastValidation} = f(\text{Temperature}, \text{ForecastBatch})$$

A stateful join that validates matured forecast hours against actual temperature
readings. The entire bridge layer (mpsc channel, `Arc<Mutex<>>`, relay tap,
reactive source) exists only because AimDB lacks a primitive for this pattern.

---

## Incident Analysis — The Bridge That Failed Silently

> **Added in v1.1 (February 9, 2026)** after a production debugging session
> that elevated this proposal from "nice to have" to "must have."

### What Happened

On February 8, 2026, the Weather Hub Pro was reflashed with commit `9ed7b05`
("feat: Implement per-city forecast validation with HourTracker and mpsc
channels"). This commit replaced the old cycle-based validation with the
reactive `.source()` + mpsc bridge pattern described in the Motivation section.

After the reflash, **zero validation results were emitted** despite ~1 hour
of stable runtime. No `✅` log lines, no `ForecastValidation` records
produced, complete silence from the validation pipeline.

### Root Cause Analysis

A deep code audit of `aimdb-core`'s spawn infrastructure confirmed:

1. **The core spawn path is sound.** `builder.rs:configure()` correctly stores
   spawn functions. `builder.rs:build()` correctly resolves keys, invokes
   spawners. `typed_record.rs:spawn_all_tasks()` correctly calls
   `spawn_producer_service()` and `spawn_consumer_tasks()`.

2. **`AccuracyKey` and `TempKey` produce different key strings** (e.g.,
   `"accuracy.vienna"` vs `"temp.vienna"`), so the `is_new_record` check in
   `configure()` works correctly — each `ForecastValidation` record is
   genuinely new and gets its own spawn function.

3. **The `tracing` feature is not enabled** in the production build
   (`aimdb-core` is used with `features = ["derive"]` only). All spawn
   debug/info messages in `typed_record.rs` are behind `#[cfg(feature =
   "tracing")]` and compiled out entirely. This means **the entire spawn
   pipeline executes silently** — no confirmation that `.source()` tasks
   actually started.

4. **The bug is in the bridge wiring**, not in core infrastructure. The
   bridge involves three async tasks and two shared-state mechanisms that
   can fail silently:

   - `relay_temps()` — a `.tap()` that relays `Temperature` values to an
     `mpsc::UnboundedSender`. If the channel receiver is dropped (e.g.,
     because `validate_forecasts` panicked during startup), the tap silently
     `break`s out of its loop.
   - `ingest_forecasts()` — a `.tap()` that writes into an
     `Arc<Mutex<HashMap>>`. Silent by design.
   - `validate_forecasts()` — a `.source()` that receives from the mpsc
     receiver. If the sender was never connected (wrong channel instance)
     or the task never started, nothing happens.

   There are at least **four failure modes** that produce the same
   observable symptom (no output):
   - mpsc channel sender/receiver mismatch (wrong instance captured)
   - `Arc<Mutex<>>` captured by wrong closure (shared state mislink)
   - `.source()` task panicked before entering the recv loop
   - Timing: mpsc receiver dropped before relay tap subscribed

   Crucially, **none of these failure modes produce any log output** because
   the `validate_forecasts` function had no startup diagnostic.

### Why This Validates the Transform Proposal

This incident demonstrates exactly the costs described in the Motivation
section — but with a real production failure:

| Theoretical Cost (Motivation) | Observed in Production |
|-------------------------------|------------------------|
| "Invisible dependencies" | AimDB had no knowledge of the Temperature → ForecastValidation link. MCP introspection showed 5 healthy Temperature records and 5 empty ForecastValidation records, with no way to see that they were supposed to be connected. |
| "User-managed synchronization" | Three async closures sharing two state mechanisms (mpsc + Arc<Mutex<>>), each capturing different cloned references. One mislink = silent total failure. |
| "Semantic confusion" | `validate_forecasts` was registered as `.source()` but was purely reactive — it blocked on `mpsc::recv()` forever if nothing fed the channel. |
| "No graph optimization" | The bug cannot be detected at build time. With `.transform_join()`, AimDB would know the input keys at registration and validate them during `build()`. |

**The bridge pattern is not just inelegant — it's a reliability hazard.**
A single mislink across 5 cities × 3 closures × 2 shared-state mechanisms
creates 30 connection points that can fail silently. `.transform_join()`
reduces this to 5 declarative registrations with build-time validation.

### Lessons for Transform Implementation

This incident directly informs several implementation requirements:

1. **Mandatory startup log (unconditional).** Transform tasks MUST log their
   start, input subscriptions, and readiness — **outside** the `#[cfg(feature
   = "tracing")]` gate. Use `#[cfg(feature = "std")]` with `eprintln!` as
   fallback, or promote key lifecycle events to always-on logging. A transform
   that starts silently and fails silently is no better than the bridge it
   replaces.

2. **Build-time input validation.** The `build()` method must verify that
   every `input_key` referenced by a `.transform()` or `.transform_join()`
   corresponds to a registered record with a configured buffer. This catches
   typos and registration-order bugs at startup rather than at runtime.

3. **Subscription failure is fatal.** If a transform task fails to subscribe
   to its input buffer (e.g., because the input record has no buffer), the
   task must **panic or return an error that propagates to `build()`** — not
   silently return. The current tap pattern of `let Ok(mut reader) =
   consumer.subscribe() else { return; }` is too lenient for transforms
   because a transform that cannot read its inputs has no reason to exist.

4. **Diagnostics in the graph.** The `DependencyGraph` (Phase 3) should
   surface enough information for MCP tools to answer: "Why is record X
   empty?" → "Its transform's input Y has not produced any values yet" or
   "Its transform task is not running." This is the introspection that was
   completely impossible with the bridge pattern.

---

## Core Principles Review

### The AimDB Data-Flow Triad

After this proposal, AimDB's registration API has three orthogonal primitives:

| Primitive | Semantics | Inputs | Output | State | Lifecycle |
|-----------|-----------|--------|--------|-------|-----------|
| `.source(fn)` | Autonomous producer | External | → Record | Opaque | Spawned task |
| `.tap(fn)` | Passive observer | Record → | Side-effect | None* | Spawned task |
| `.transform(fn)` | Reactive derivation | Record(s) → | → Record | Managed | Spawned task |

\* Taps should remain **stateless side-effect observers**. If a tap accumulates
state that feeds another record, it should be refactored into a `.transform()`.

**The litmus test:**

- Does it read from the **external world** (sensor, timer, API)? → `.source()`
- Does it **observe** a record without producing data? → `.tap()`
- Does it **derive** a record from other record(s)? → `.transform()`

### Relationship to `.link()`

`.link_from()` / `.link_to()` connect records to **external systems** via
connectors (MQTT, KNX, etc.). They handle serialization, protocol details,
and network I/O.

`.transform()` connects records to **other records** within the same AimDB
instance. It handles computation, state management, and type conversion.

They are complementary:

```
External ──link_from()──► Record A ──transform()──► Record B ──link_to()──► External
                            │                          │
                          tap()                      tap()
                            ▼                          ▼
                        Side-effect               Side-effect
```

---

## Problem Analysis

### Three Transform Archetypes

Analysis of the Weather Hub and other use cases reveals three recurring
patterns, listed in order of increasing complexity:

#### 1. Map (1:1, stateless)

Transform each value of type `A` into a value of type `B`:

```
Record<Temperature> ──map──► Record<TemperatureAlert>
```

**Example:** Derive an alert record whenever the temperature crosses a
threshold. Each input value produces zero or one output value — pure
stateless logic, no accumulation, no external I/O.

> **Note on `ws_tap`:** The Weather Hub's `ws_tap` looks superficially like
> a map (`T → WsMessage`), but it is really a **connector** concern — its
> job is to push data to an external WebSocket transport. The proper solution
> is a future WebSocket `.link_to()` connector, not a `.transform()`.
> Transforms derive **records from records**; connectors move data to/from
> **external systems**.

#### 2. Accumulate (N:1, stateful, single-input)

Aggregate a stream of values into derived state:

```
Record<ForecastBatch> ──accumulate──► Record<ForecastSummary>
```

**Current workaround:** `ingest_forecasts` tap mutates an
`Arc<Mutex<HashMap<u64, HourTracker>>>` — a stateful accumulation that
escapes AimDB's model.

#### 3. Join (M×N:1, stateful, multi-input)

Combine values from multiple input records:

```
Record<Temperature> ─┐
                     ├──join──► Record<ForecastValidation>
Record<ForecastBatch>┘
```

**Current workaround:** The entire bridge machinery (mpsc channel + shared
mutex + relay tap + reactive source) in the Weather Hub.

### Design Decision: Unified API

Rather than three separate methods (`.map()`, `.accumulate()`, `.join()`),
we propose a **single `.transform()` method** that handles all three archetypes
through its closure signature. The stateless map is simply a transform with
`()` state; the single-input accumulate is a join with one input.

This keeps the API surface minimal and avoids the combinatorial explosion of
specialized methods.

---

## Design Constraints

1. **`no_std`-ready design** — The core transform types must not preclude a
   future `no_std`/Embassy port, but **Embassy support is out of scope** for
   this proposal. Initial implementation targets Tokio only.
2. **Runtime-agnostic trait surface** — Uses the existing
   `impl_record_registrar_ext!` macro pattern so Embassy can be added later
   without API changes
3. **Type-safe inputs** — Each input record's type is known at compile time;
   the transform closure receives typed values, not `dyn Any`
4. **At most one source OR transform per record** — A record cannot have
   both a `.source()` and a `.transform()`. This is enforced at registration
   time (panic, matching `.source()` behavior)
5. **Multiple taps still allowed** — Taps observe the transform's output just
   like they observe a source's output
6. **Backpressure-aware** — Transforms consume from input buffers at their own
   pace; slow transforms don't block input producers (SPMC semantics preserved)
7. **Introspectable** — Transform edges appear in the AimDB dependency graph,
   queryable via AimX/MCP
8. **Zero overhead when unused** — No cost if `.transform()` is never called
9. **Composable** — A transform's output record can be another transform's input
   (DAG, not just one level deep)

---

## API Design

### Single-Input Transform

The simplest and most common case: derive record B from record A.

```rust
// Stateless map: Temperature → TemperatureAlert
builder.configure::<TemperatureAlert>(AlertKey::Vienna, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .transform(TempKey::Vienna, |ctx| {
           ctx.map(|temp: &Temperature| {
               if temp.celsius > 35.0 {
                   Some(TemperatureAlert { celsius: temp.celsius, level: "high" })
               } else {
                   None  // no output for this input
               }
           })
       });
});

// Stateful accumulate: ForecastBatch → ForecastSummary
builder.configure::<ForecastSummary>(SummaryKey::Vienna, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 50 })
       .transform(ForecastKey::Vienna24h, |ctx| {
           ctx.with_state(HashMap::<u64, HourTracker>::new())
              .on_value(|batch: &ForecastBatch, state| {
                  for entry in &batch.entries {
                      state.entry(entry.valid_for)
                           .and_modify(|t| t.update(entry.temp_celsius, batch.fetched_at))
                           .or_insert_with(|| HourTracker::new(entry.temp_celsius, batch.fetched_at));
                  }
                  // Emit updated summary
                  Some(ForecastSummary::from_trackers(state))
              })
       });
});
```

### Multi-Input Transform (Join)

The motivating use case: derive record C from records A and B.

```rust
// Stateful join: Temperature × ForecastBatch → ForecastValidation
builder.configure::<ForecastValidation>(AccuracyKey::Vienna, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 500 })
       .tap(move |ctx, consumer| ws_tap(ctx, consumer, tx.clone(), "validation.vienna"))
       .transform_join(|join| {
           join.input::<Temperature>(TempKey::Vienna)
               .input::<ForecastBatch>(ForecastKey::Vienna24h)
               .with_state(ValidationState::new(tolerance_ms))
               .on_trigger(|trigger, state, producer| async move {
                   match trigger {
                       Trigger::Input::<Temperature>(temp) => {
                           state.push_temp(temp);
                           // Check matured forecasts against new reading
                           for validation in state.validate_matured() {
                               producer.produce(validation).await;
                           }
                       }
                       Trigger::Input::<ForecastBatch>(batch) => {
                           state.ingest_batch(batch);
                       }
                   }
               })
       });
});
```

### Method Signatures

#### On `RecordRegistrar` (low-level, runtime-agnostic)

```rust
impl<'a, T, R> RecordRegistrar<'a, T, R>
where
    T: Send + Sync + Clone + Debug + 'static,
    R: Spawn + 'static,
{
    /// Register a single-input transform. Panics if a source or transform
    /// is already registered for this record.
    ///
    /// `input_key`: The record key to subscribe to as input.
    /// `build_fn`: Closure that configures the transform pipeline via a
    ///             `TransformBuilder`.
    pub fn transform_raw<I, F>(
        &'a mut self,
        input_key: impl RecordKey,
        build_fn: F,
    ) -> &'a mut Self
    where
        I: Send + Sync + Clone + Debug + 'static,
        F: FnOnce(TransformBuilder<I, T>) -> TransformPipeline<I, T, R> + Send + 'static;

    /// Register a multi-input transform (join). Panics if a source or
    /// transform is already registered for this record.
    pub fn transform_join_raw<F>(
        &'a mut self,
        build_fn: F,
    ) -> &'a mut Self
    where
        F: FnOnce(JoinBuilder<T, R>) -> JoinPipeline<T, R> + Send + 'static;
}
```

#### On the generated extension trait (e.g., `TokioRecordRegistrarExt`)

```rust
pub trait TokioRecordRegistrarExt<'a, T> {
    // ... existing source(), tap(), buffer() ...

    /// Single-input reactive transform.
    fn transform<I, F>(
        &'a mut self,
        input_key: impl RecordKey,
        build_fn: F,
    ) -> &'a mut RecordRegistrar<'a, T, TokioAdapter>
    where
        I: Send + Sync + Clone + Debug + 'static,
        F: FnOnce(TransformBuilder<I, T>) -> TransformPipeline<I, T, TokioAdapter>
            + Send + 'static;

    /// Multi-input reactive transform (join).
    fn transform_join<F>(
        &'a mut self,
        build_fn: F,
    ) -> &'a mut RecordRegistrar<'a, T, TokioAdapter>
    where
        F: FnOnce(JoinBuilder<T, TokioAdapter>) -> JoinPipeline<T, TokioAdapter>
            + Send + 'static;
}
```

### Builder Types

```rust
/// Configures a single-input transform pipeline.
pub struct TransformBuilder<I, O> {
    _phantom: PhantomData<(I, O)>,
}

impl<I, O> TransformBuilder<I, O>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
{
    /// Stateless 1:1 map. Returning `None` skips output for this input.
    pub fn map<F>(self, f: F) -> TransformPipeline<I, O, R>
    where
        F: Fn(&I) -> Option<O> + Send + Sync + 'static;

    /// Stateful transform. `S` is the user-defined state type.
    pub fn with_state<S: Send + 'static>(self, initial: S) -> StatefulTransformBuilder<I, O, S>;
}

impl<I, O, S> StatefulTransformBuilder<I, O, S>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
{
    /// Called for each input value. Receives mutable state, returns optional output.
    pub fn on_value<F>(self, f: F) -> TransformPipeline<I, O, R>
    where
        F: Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static;
}
```

```rust
/// Configures a multi-input join transform.
pub struct JoinBuilder<O, R: Spawn + 'static> {
    inputs: Vec<JoinInput<R>>,
    _phantom: PhantomData<O>,
}

impl<O, R> JoinBuilder<O, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    R: Spawn + 'static,
{
    /// Add a typed input to the join.
    pub fn input<I: Send + Sync + Clone + Debug + 'static>(
        mut self,
        key: impl RecordKey,
    ) -> Self;

    /// Set the join state and trigger handler.
    pub fn with_state<S: Send + 'static>(self, initial: S) -> JoinStateBuilder<O, S, R>;
}

impl<O, S, R> JoinStateBuilder<O, S, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: Spawn + 'static,
{
    /// Async handler called whenever any input produces a value.
    /// Receives a type-safe trigger enum, mutable state, and a producer.
    pub fn on_trigger<F, Fut>(self, f: F) -> JoinPipeline<O, R>
    where
        F: Fn(JoinTrigger, &mut S, &Producer<O, R>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static;
}
```

### The Trigger Enum

For multi-input joins, the user needs to know which input fired. Rather than
a single `dyn Any` value, we use a builder-generated approach:

```rust
/// Tells the join handler which input produced a value.
///
/// Users match on this to dispatch typed handling.
/// Input index corresponds to the order of `.input()` calls.
pub enum JoinTrigger {
    /// Input at the given index fired with a type-erased value.
    /// Use `.downcast_ref::<T>()` to recover the typed value.
    Input { index: usize, value: Box<dyn Any + Send> },
}

impl JoinTrigger {
    /// Convenience: try to downcast as the expected input type.
    pub fn as_input<T: 'static>(&self) -> Option<&T>;
}
```

**Usage in the handler:**

```rust
.on_trigger(|trigger, state, producer| async move {
    if let Some(temp) = trigger.as_input::<Temperature>() {
        state.push_temp(temp.clone());
        for v in state.validate_matured() {
            let _ = producer.produce(v).await;
        }
    } else if let Some(batch) = trigger.as_input::<ForecastBatch>() {
        state.ingest_batch(batch.clone());
    }
})
```

---

## Architecture

### Component Diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│  AimDB Instance                                                      │
│                                                                      │
│  ┌─────────────────────┐       ┌─────────────────────────┐          │
│  │ Record A (source)   │       │ Record B (source/link)  │          │
│  │ e.g. Temperature    │       │ e.g. ForecastBatch      │          │
│  │                     │       │                         │          │
│  │  buffer: SpmcRing   │       │  buffer: SpmcRing       │          │
│  └──────┬──────────────┘       └──────┬──────────────────┘          │
│         │                             │                              │
│         │ subscribe()                 │ subscribe()                  │
│         │                             │                              │
│  ┌──────▼─────────────────────────────▼──────────────────────────┐  │
│  │                  TransformTask                                │  │
│  │                                                               │  │
│  │   Spawned during build(). Owns:                               │  │
│  │   ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │  │
│  │   │ ReaderA (sub) │  │ ReaderB (sub) │  │ State S (owned) │   │  │
│  │   └──────┬───────┘  └──────┬───────┘  └──────────────────┘   │  │
│  │          │                 │                                   │  │
│  │          └────┬────────────┘                                   │  │
│  │               │ select! { a = readerA.recv(), b = ... }       │  │
│  │               ▼                                               │  │
│  │     ┌─────────────────────┐                                   │  │
│  │     │ User closure(       │                                   │  │
│  │     │   trigger,          │                                   │  │
│  │     │   &mut state,       │                                   │  │
│  │     │   &producer         │                                   │  │
│  │     │ )                   │                                   │  │
│  │     └──────────┬──────────┘                                   │  │
│  │                │ producer.produce(output)                     │  │
│  └────────────────┼──────────────────────────────────────────────┘  │
│                   ▼                                                  │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ Record C (transform output)                                  │    │
│  │ e.g. ForecastValidation                                      │    │
│  │                                                              │    │
│  │  buffer: SpmcRing                                            │    │
│  │     │                                                        │    │
│  │     ├──► tap: ws_tap()  → WebSocket                          │    │
│  │     ├──► tap: metrics() → Prometheus                         │    │
│  │     └──► [further transforms, links, etc.]                   │    │
│  └──────────────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────────┘
```

### Data Flow (Single-Input)

```
Input record buffer
       │
       │ BufferReader::recv()
       ▼
  TransformTask loop:
       │
       ├── Receives value of type I
       │
       ├── Calls user closure: fn(&I, &mut S) -> Option<O>
       │                           │
       │                     ┌─────┴─────┐
       │                     │           │
       │                   Some(o)     None
       │                     │           │
       │              producer.produce(o) (skip)
       │                     │
       ▼                     ▼
  Output record buffer ◄────┘
```

### Data Flow (Multi-Input Join)

```
Input A buffer ───► ReaderA ──┐
                              │
Input B buffer ───► ReaderB ──┤  select! (race)
                              │
Input C buffer ───► ReaderC ──┘
                              │
                              ▼
                    TransformTask identifies trigger
                              │
                              ▼
                    User closure: fn(JoinTrigger, &mut S, &Producer<O, R>)
                              │
                              ▼  (zero or more calls)
                    producer.produce(output_value)
                              │
                              ▼
                    Output record buffer
```

---

## Implementation

### 7.1 Transform Descriptor Storage

Transforms are stored alongside source and tap descriptors in `TypedRecord`.
The actual field layout follows the existing pattern of `producer_service` and
`consumers` — wrapped in platform-conditional `Mutex` for interior mutability
(needed to `take()` during spawning):

```rust
// aimdb-core/src/typed_record.rs

pub struct TypedRecord<T: Send + 'static + Debug + Clone, R: Spawn + 'static> {
    #[cfg(feature = "std")]
    producer_service: std::sync::Mutex<Option<ProducerServiceFn<T, R>>>,
    #[cfg(not(feature = "std"))]
    producer_service: spin::Mutex<Option<ProducerServiceFn<T, R>>>,

    #[cfg(feature = "std")]
    consumers: std::sync::Mutex<Vec<ConsumerServiceFn<T, R>>>,
    #[cfg(not(feature = "std"))]
    consumers: spin::Mutex<Vec<ConsumerServiceFn<T, R>>>,

    /// NEW: Transform descriptor, mutually exclusive with producer_service.
    /// Uses the same Mutex pattern for take()-during-spawn.
    #[cfg(feature = "std")]
    transform: std::sync::Mutex<Option<TransformDescriptor<T, R>>>,
    #[cfg(not(feature = "std"))]
    transform: spin::Mutex<Option<TransformDescriptor<T, R>>>,

    buffer: Option<Box<dyn DynBuffer<T>>>,
    // ... existing fields unchanged
}
```

The `TransformDescriptor` stores the input keys and a type-erased spawn function.
Unlike the original design which used a fully erased `T_erased`, we keep `T`
(the output type) in the descriptor because it's already scoped to the
`TypedRecord<T, R>` that owns it:

```rust
/// Transform descriptor, stored per output record.
///
/// The spawn function captures all type information (input types, state type)
/// in its closure and only needs the `AimDb` handle and runtime context at
/// spawn time — matching the existing `ProducerServiceFn` pattern.
pub(crate) struct TransformDescriptor<T, R: Spawn + 'static>
where
    T: Send + 'static + Debug + Clone,
{
    /// Record keys this transform subscribes to (for build-time validation).
    pub input_keys: Vec<String>,
    /// Spawn function: takes (Producer<T, R>, Arc<AimDb<R>>, runtime_ctx) → Future.
    /// Matches the ProducerServiceFn pattern exactly.
    pub spawn_fn: Box<
        dyn FnOnce(crate::Producer<T, R>, Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, ()>
            + Send
            + Sync,
    >,
    /// The AimDb handle, set during build() before spawning.
    /// This is needed because transforms subscribe to *other* records' buffers,
    /// requiring the fully-constructed database handle.
    pub db: Option<Arc<AimDb<R>>>,
}
```

> **Design note:** The `spawn_fn` signature intentionally mirrors
> `ProducerServiceFn<T, R>` (which is `FnOnce(Producer<T, R>, Arc<dyn Any>) →
> BoxFuture`). This means the spawning logic in `RecordSpawner` can treat
> transforms and sources almost identically, reducing implementation risk.

### 7.2 Registration-Time Validation

When `.transform()` or `.transform_join()` is called:

```rust
pub fn set_transform(&self, descriptor: TransformDescriptor<T, R>) {
    // Enforce mutual exclusion with .source()
    #[cfg(feature = "std")]
    let has_source = self.producer_service.lock().unwrap().is_some();
    #[cfg(not(feature = "std"))]
    let has_source = self.producer_service.lock().is_some();

    if has_source {
        panic!(
            "Record already has a .source(); cannot also have a .transform(). \
             A record's values come from exactly one origin."
        );
    }

    #[cfg(feature = "std")]
    let mut slot = self.transform.lock().unwrap();
    #[cfg(not(feature = "std"))]
    let mut slot = self.transform.lock();

    if slot.is_some() {
        panic!("Record already has a .transform(); only one is allowed.");
    }
    *slot = Some(descriptor);
}
```

Symmetrically, `set_producer_service` must check for existing transforms.

> **Code gap (v1.1 note):** The current `set_producer_service()` implementation
> in `typed_record.rs` (line ~583) does NOT check for existing transforms —
> it only checks if a producer is already set. This must be updated when
> adding `.transform()` support to enforce the mutual exclusion invariant
> bidirectionally:

```rust
pub fn set_producer_service<F, Fut>(&mut self, f: F)
where
    F: FnOnce(crate::Producer<T, R>, Arc<dyn Any + Send + Sync>) -> Fut
        + Send + Sync + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
{
    // NEW: Check for existing transform
    #[cfg(feature = "std")]
    let has_transform = self.transform.lock().unwrap().is_some();
    #[cfg(not(feature = "std"))]
    let has_transform = self.transform.lock().is_some();

    if has_transform {
        panic!("Record already has a .transform(); cannot also have a .source().");
    }

    // Existing: Check if already set
    // ...
}
```

**Build-time input key validation** (from [Incident Lesson #2](#lessons-for-transform-implementation)):

```rust
// In build(), after all records are registered but before spawning:
for (output_key, record) in &self.records {
    if let Some(descriptor) = record.transform_descriptor() {
        for input_key in &descriptor.input_keys {
            if !registered_keys.contains(input_key) {
                return Err(DbError::MissingConfiguration {
                    parameter: format!(
                        "Transform on '{}' references unknown input record '{}'. \
                         Available records: {:?}",
                        output_key, input_key,
                        registered_keys
                    ),
                });
            }
        }
    }
}
```

### 7.3 Build-Time Spawning

During `AimDb::build()`, after all records are constructed and the `AimDb`
handle exists, transform tasks are spawned. The existing spawn path uses
`RecordSpawner<T>::spawn_all_tasks()` which is called from a type-erased
closure stored in `builder.spawn_fns`. We extend this method:

```rust
// In RecordSpawner<T>::spawn_all_tasks()

// Existing: spawn producer service (if any)
if typed_record.has_producer_service() {
    typed_record.spawn_producer_service(runtime, db, record_key)?;
}

// NEW: spawn transform task (if any) — mutually exclusive with source
if typed_record.has_transform() {
    typed_record.spawn_transform_task(runtime, db, record_key)?;
}

// Existing: spawn consumer tasks (taps) — these work on both source
// and transform output
if typed_record.consumer_count() > 0 {
    typed_record.spawn_consumer_tasks(runtime, db, record_key)?;
}
```

The `spawn_transform_task` method follows the exact same pattern as
`spawn_producer_service` (see `typed_record.rs` line ~894):

```rust
pub fn spawn_transform_task(
    &self,
    runtime: &Arc<R>,
    db: &Arc<crate::AimDb<R>>,
    record_key: &str,
) -> crate::DbResult<()>
where
    R: aimdb_executor::Spawn,
    T: Sync,
{
    use crate::DbError;

    // Take the transform descriptor (can only spawn once)
    let descriptor = {
        #[cfg(feature = "std")]
        { self.transform.lock().unwrap().take() }
        #[cfg(not(feature = "std"))]
        { self.transform.lock().take() }
    };

    if let Some(mut desc) = descriptor {
        // OBSERVABILITY (Incident Lesson #1): Always log transform start.
        // This is NOT gated behind #[cfg(feature = "tracing")] because
        // the production incident showed that silent spawning = silent failure.
        #[cfg(feature = "tracing")]
        tracing::info!(
            "🔄 Spawning transform task for '{}' (inputs: {:?})",
            record_key, desc.input_keys
        );

        // Inject the db handle so the transform can subscribe to inputs
        desc.db = Some(db.clone());

        // Create Producer<T> bound to the specific record key
        let producer = crate::typed_api::Producer::new(
            db.clone(), record_key.to_string()
        );
        let ctx: Arc<dyn core::any::Any + Send + Sync> = runtime.clone();

        // Call the spawn function (mirrors spawn_producer_service exactly)
        let task_future = (desc.spawn_fn)(producer, ctx);
        runtime.spawn(task_future).map_err(DbError::from)?;
    }

    Ok(())
}
```

> **Key difference from source spawning:** The transform's `spawn_fn` captures
> the `AimDb` handle (via `desc.db`) to subscribe to input buffers. A source's
> `producer_service` only needs the `Producer<T, R>` and runtime context because
> it reads from external systems, not from other records.

### 7.4 Single-Input Transform Task

The generated async task for a single-input transform. Note: uses
`aimdb_executor::Spawn` (the actual trait bound), not `Runtime`:

```rust
/// Spawned task for a single-input stateful transform.
async fn run_single_transform<I, O, S, R, F>(
    db: Arc<AimDb<R>>,
    input_key: String,
    output_key: String,
    producer: Producer<O, R>,
    mut state: S,
    transform_fn: F,
)
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: aimdb_executor::Spawn + 'static,
    F: Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
{
    // OBSERVABILITY (Incident Lesson #1): Always confirm startup.
    #[cfg(feature = "tracing")]
    tracing::info!(
        "🔄 Transform started: '{}' → '{}'",
        input_key, output_key
    );

    // Subscribe to the input record's buffer.
    // Incident Lesson #3: subscription failure is FATAL for transforms.
    let consumer = Consumer::<I, R>::new(db.clone(), input_key.clone());
    let mut reader = match consumer.subscribe() {
        Ok(r) => r,
        Err(e) => {
            // This is a configuration error — always log, never silently return.
            #[cfg(feature = "tracing")]
            tracing::error!(
                "❌ Transform '{}' → '{}' FAILED to subscribe to input: {:?}",
                input_key, output_key, e
            );
            #[cfg(all(feature = "std", not(feature = "tracing")))]
            eprintln!(
                "[aimdb] FATAL: Transform '{}' → '{}' failed to subscribe: {:?}",
                input_key, output_key, e
            );
            return;
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Transform '{}' → '{}' subscribed, entering event loop",
        input_key, output_key
    );

    // React to each input value
    while let Ok(input_value) = reader.recv().await {
        if let Some(output_value) = transform_fn(&input_value, &mut state) {
            let _ = producer.produce(output_value).await;
        }
    }

    #[cfg(feature = "tracing")]
    tracing::warn!(
        "🔄 Transform '{}' → '{}' input closed, task exiting",
        input_key, output_key
    );
}
```

> **Why `eprintln!` fallback?** The production incident showed that with
> `#[cfg(feature = "tracing")]` compiled out, fatal errors vanish entirely.
> For transforms — which replace error-prone bridge wiring — a bare minimum
> `eprintln!` on subscription failure ensures the operator sees something
> even without the tracing feature. This is a **defense-in-depth** choice
> specific to transforms; existing `.source()` and `.tap()` patterns can
> adopt this later if desired.

### 7.5 Multi-Input Join Task

The join task uses `tokio::select!` (or embassy equivalent) to race on
multiple input readers. The recommended approach (see [Open Point #1](#1-type-erasure-for-multi-input-joins))
uses per-input forwarding tasks that send typed `JoinTrigger` values into a
single `mpsc::channel`:

```rust
/// Spawned task for a multi-input join transform.
///
/// Architecture: spawns N lightweight forwarder tasks (one per input), each
/// subscribing to its input buffer and forwarding type-erased JoinTrigger
/// values to a shared mpsc channel. The main task reads from this channel.
///
/// This avoids trait object gymnastics with BufferReader and works naturally
/// with both Tokio and Embassy (future).
async fn run_join_transform<O, S, R, F, Fut>(
    db: Arc<AimDb<R>>,
    inputs: Vec<JoinInputDescriptor>,
    output_key: String,
    producer: Producer<O, R>,
    mut state: S,
    handler: F,
)
where
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: aimdb_executor::Spawn + 'static,
    F: Fn(JoinTrigger, &mut S, &Producer<O, R>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    // OBSERVABILITY: Always confirm startup
    let input_keys: Vec<_> = inputs.iter().map(|i| i.key.clone()).collect();
    #[cfg(feature = "tracing")]
    tracing::info!(
        "🔄 Join transform started: {:?} → '{}'",
        input_keys, output_key
    );

    // Create the shared trigger channel
    let (trigger_tx, mut trigger_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn per-input forwarder tasks
    for (index, input) in inputs.into_iter().enumerate() {
        let tx = trigger_tx.clone();
        let db = db.clone();

        // Each forwarder subscribes to one input and sends JoinTrigger values
        let forwarder = (input.subscribe_and_forward_fn)(db, index, tx);
        if let Err(e) = R::spawn(forwarder) {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "❌ Join transform '{}' failed to spawn forwarder for input '{}': {:?}",
                output_key, input.key, e
            );
            return;
        }
    }

    // Drop our copy of the sender — when all forwarders exit, the channel closes
    drop(trigger_tx);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Join transform '{}' all forwarders spawned, entering event loop",
        output_key
    );

    // Event loop: dispatch typed triggers to the user handler
    while let Some(trigger) = trigger_rx.recv().await {
        handler(trigger, &mut state, &producer).await;
    }

    #[cfg(feature = "tracing")]
    tracing::warn!(
        "🔄 Join transform '{}' all inputs closed, task exiting",
        output_key
    );
}
```

**Note on `select!` and `no_std`:** The multi-input join requires a runtime
`select!` mechanism. On Tokio this is `tokio::select!`. On Embassy this is
`embassy_futures::select`. The implementation is behind the runtime adapter.
For the initial implementation, multi-input joins are `std`-only. Single-input
transforms work on `no_std`.

### 7.6 Input Record Resolution

Transform inputs reference records by key. These keys are resolved during
`build()`, after all records have been registered. This means:

- Input records **must** be registered before `build()` is called
- Circular dependencies are detected at build time (see [Dependency Graph](#dependency-graph))
- Missing input keys produce a clear error at build time

```rust
// During build(), validate transform inputs exist
for (output_key, transform) in &transform_descriptors {
    for input_key in &transform.input_keys {
        if !registered_keys.contains(input_key) {
            return Err(DbError::Configuration(format!(
                "Transform on '{}' references unknown input record '{}'",
                output_key, input_key
            )));
        }
    }
}
```

---

## Dependency Graph

With transforms, AimDB records form a **directed acyclic graph (DAG)**. This
graph is a first-class concept — constructed at build time, validated for
cycles, and exposed over the AimX protocol for remote introspection.

### Graph Model

Every record is a **node**. Edges represent data dependencies:

| Edge Type | Source | Target | Created By |
|-----------|--------|--------|------------|
| `source` | — (external) | Record | `.source()` |
| `link` | — (external) | Record | `.link_from()` |
| `transform` | Record | Record | `.transform()` |
| `transform_join` | Record(s) | Record | `.transform_join()` |
| `tap` | Record | — (side-effect) | `.tap()` |
| `link_out` | Record | — (external) | `.link_to()` |

**Node metadata** (stored per record at build time):

```rust
/// Metadata for one node in the dependency graph.
#[derive(Clone, Debug, Serialize)]
pub struct GraphNode {
    /// Record key (e.g. "temp.vienna")
    pub key: String,
    /// How this record gets its values
    pub origin: RecordOrigin,
    /// Buffer type ("spmc_ring", "single_latest", "mailbox")
    pub buffer_type: String,
    /// Buffer capacity (None for unbounded)
    pub buffer_capacity: Option<usize>,
    /// Number of taps attached
    pub tap_count: usize,
    /// Whether an outbound link is configured
    pub has_outbound_link: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrigin {
    /// Autonomous producer via `.source()`
    Source,
    /// Inbound connector via `.link_from()`
    Link { protocol: String, address: String },
    /// Single-input reactive derivation via `.transform()`
    Transform { input: String },
    /// Multi-input reactive join via `.transform_join()`
    TransformJoin { inputs: Vec<String> },
    /// No registered producer (writable via `record.set` / `db.produce()`)
    Passive,
}
```

**Edge metadata** (derived from nodes at build time):

```rust
/// One directed edge in the dependency graph.
#[derive(Clone, Debug, Serialize)]
pub struct GraphEdge {
    /// Source record key (None for external origins)
    pub from: Option<String>,
    /// Target record key (None for side-effects/outbound)
    pub to: Option<String>,
    /// Classification of this edge
    pub edge_type: EdgeType,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Source,
    Link { protocol: String },
    Transform,
    TransformJoin,
    Tap { index: usize },
    LinkOut { protocol: String },
}
```

### Internal Representation

The graph is built during `AimDb::build()` and stored as an immutable
adjacency list inside `AimDbInner`:

```rust
/// The dependency graph, constructed once during build() and immutable thereafter.
pub(crate) struct DependencyGraph {
    /// All nodes indexed by record key.
    pub nodes: HashMap<String, GraphNode>,
    /// All edges (both internal and external).
    pub edges: Vec<GraphEdge>,
    /// Topological order of record keys (transforms come after their inputs).
    pub topo_order: Vec<String>,
}
```

### Build-Time DAG Validation

The `build()` method validates the graph before spawning any tasks:

```rust
impl DependencyGraph {
    /// Construct and validate the dependency graph from registered records.
    ///
    /// Returns `Err(DbError::CyclicDependency)` if the transform edges
    /// form a cycle.
    pub fn build(records: &HashMap<String, RecordMeta>) -> DbResult<Self> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

        // Build adjacency list from transform input_keys
        for (output_key, meta) in records {
            in_degree.entry(output_key).or_insert(0);
            for input_key in &meta.transform_inputs {
                adjacency.entry(input_key).or_default().push(output_key);
                *in_degree.entry(output_key).or_insert(0) += 1;
            }
        }

        // Kahn's algorithm — topological sort
        let mut queue: VecDeque<&str> = in_degree.iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut topo_order = Vec::new();

        while let Some(node) = queue.pop_front() {
            topo_order.push(node.to_string());
            if let Some(dependents) = adjacency.get(node) {
                for dep in dependents {
                    let deg = in_degree.get_mut(dep).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep);
                    }
                }
            }
        }

        if topo_order.len() != records.len() {
            // Find the cycle for a helpful error message
            let in_cycle: Vec<_> = in_degree.iter()
                .filter(|(_, &deg)| deg > 0)
                .map(|(k, _)| (*k).to_string())
                .collect();
            return Err(DbError::CyclicDependency {
                records: in_cycle,
            });
        }

        Ok(Self {
            nodes: /* ... build from metadata ... */,
            edges: /* ... derive from nodes ... */,
            topo_order,
        })
    }
}
```

**New error variant:**

```rust
pub enum DbError {
    // ... existing variants ...

    /// Transform dependency graph contains a cycle.
    CyclicDependency { records: Vec<String> },
}
```

**Error message example:**

```
Error: Cyclic dependency detected among records: ["metrics.cpu", "alert.cpu", "metrics.cpu"]
Help: A .transform() chain must form a DAG. Record "alert.cpu" transforms from
      "metrics.cpu", which transforms from "alert.cpu", creating a cycle.
```

### Input Key Resolution

Transform inputs reference records by key. These keys are resolved during
`build()` after all records have been registered:

```rust
// During build(), validate all transform inputs exist
for (output_key, meta) in &record_metas {
    for input_key in &meta.transform_inputs {
        if !record_metas.contains_key(input_key) {
            return Err(DbError::Configuration(format!(
                "Transform on '{}' references unknown input record '{}'. \
                 Available records: {:?}",
                output_key, input_key,
                record_metas.keys().collect::<Vec<_>>()
            )));
        }
    }
}
```

---

## AimX & MCP Exposure

The dependency graph is exposed to external tools via the AimX protocol and
MCP tooling layer. This is covered in a **companion design document**:

→ **[021-M9 — Graph Introspection over AimX](021-M9-graph-introspection.md)**

That document defines:
- `graph.describe`, `graph.lineage`, `graph.dot` AimX methods
- `record.list` origin extension
- `mcp_aimdb_get_graph` and `mcp_aimdb_get_lineage` MCP tools
- Client library extensions
- Testing and implementation plan for the introspection layer

The internal `DependencyGraph` types defined in the section above are the
foundation that 021 builds upon.

---

## Testing Strategy

### Unit Tests

```rust
#[tokio::test]
async fn single_input_stateless_map() {
    // Configure: Record<i32> --map(*2)--> Record<i32>
    // Produce 5 into input → verify 10 appears in output
}

#[tokio::test]
async fn single_input_stateful_accumulate() {
    // Configure: Record<i32> --sum--> Record<i64>
    // Produce [1, 2, 3] → verify [1, 3, 6] in output
}

#[tokio::test]
async fn single_input_filter() {
    // Configure: Record<i32> --filter(>5)--> Record<i32>
    // Produce [3, 7, 2, 9] → verify [7, 9] in output
}

#[tokio::test]
async fn multi_input_join() {
    // Configure: Record<A> × Record<B> --join--> Record<C>
    // Produce A(1), B(2), A(3) → verify handler called with correct triggers
}

#[tokio::test]
async fn transform_chain() {
    // A --transform--> B --transform--> C
    // Verify end-to-end propagation
}

#[tokio::test]
#[should_panic(expected = "already has a .source()")]
async fn source_and_transform_mutual_exclusion() {
    // Configure a record with both .source() and .transform()
    // Expect panic at registration time
}

#[tokio::test]
async fn cycle_detection() {
    // A → B → A (cycle)
    // Expect error from build()
}
```

### Integration Tests

```rust
#[tokio::test]
async fn weather_hub_validation_as_transform() {
    // Port the Weather Hub's forecast validation pipeline to use .transform_join()
    // Verify identical ForecastValidation outputs
    // Confirm no mpsc channels, no Arc<Mutex<>>, no relay taps needed
}
```

> **Note:** AimX graph introspection tests (graph.describe, graph.lineage,
> graph.dot, record.list origin) are defined in
> [021-M9-graph-introspection](021-M9-graph-introspection.md).

---

## Implementation Plan

> **Priority note (v1.1):** The [production incident](#incident-analysis--the-bridge-that-failed-silently)
> elevates Phases 1–2 to **critical priority**. The bridge pattern is actively
> broken in production; `.transform_join()` is the fix, not just an improvement.
> Phase 4 (Weather Hub migration) should follow immediately after Phase 2 to
> eliminate the bridge code.

### Phase 1: Core Types and Single-Input Transform

**Estimated effort:** 3–4 days

- [ ] Add `TransformDescriptor<T, R>` to `TypedRecord` (with platform-conditional Mutex)
- [ ] Implement `TransformBuilder`, `StatefulTransformBuilder`, `TransformPipeline`
- [ ] Implement `run_single_transform` async task **with mandatory startup logging**
- [ ] Add mutual exclusion: `set_transform` checks source, `set_producer_service` checks transform
- [ ] Add `has_transform()` and `spawn_transform_task()` to `TypedRecord`
- [ ] Extend `RecordSpawner::spawn_all_tasks()` for transforms
- [ ] Add `transform_raw()` to `RecordRegistrar`
- [ ] Extend `impl_record_registrar_ext!` macro with `transform()`
- [ ] **Build-time input key validation** in `build()` (incident lesson #2)
- [ ] **Subscription failure = fatal error** in transform tasks (incident lesson #3)
- [ ] Unit tests for map, accumulate, filter patterns

### Phase 2: Multi-Input Join

**Estimated effort:** 3–4 days

- [ ] Implement `JoinBuilder`, `JoinStateBuilder`, `JoinPipeline`
- [ ] Implement `JoinTrigger` enum with `as_input::<T>()` downcast
- [ ] Implement per-input forwarder tasks (recommended approach for type erasure)
- [ ] Implement `run_join_transform` with mpsc-based event loop
- [ ] Add `transform_join_raw()` to `RecordRegistrar`
- [ ] Extend macro with `transform_join()`
- [ ] Build-time input key resolution and error reporting
- [ ] Unit tests for 2-input and 3-input joins

### Phase 3: Dependency Graph

**Estimated effort:** 2–3 days

- [ ] Implement `DependencyGraph`, `GraphNode`, `GraphEdge`, `RecordOrigin` types
- [ ] Populate `RecordOrigin` from `.source()`, `.link_from()`, `.transform()`,
  `.transform_join()` during registration
- [ ] Implement DAG construction in `build()`
- [ ] Cycle detection (Kahn's algorithm) with helpful error messages
- [ ] Topological sort for spawn ordering
- [ ] Add `CyclicDependency` error variant to `DbError`
- [ ] Store `DependencyGraph` as immutable field in `AimDbInner`
- [ ] Unit tests: valid DAGs, cycle detection, missing input keys

### Phase 4: Weather Hub Migration (critical — eliminates the broken bridge)

**Estimated effort:** 1 day

- [ ] Refactor Weather Hub to use `.transform_join()` for forecast validation
- [ ] Remove bridge taps (`relay_temps`), mpsc channels, `Arc<Mutex<>>`
- [ ] Remove `ingest_forecasts` tap (absorbed into transform state)
- [ ] Verify identical behavior (compare validation output)
- [ ] Confirm validation logs appear after reflash (**the original incident**)
- [ ] Document before/after comparison

> **Phases 5–6 (AimX Protocol + MCP Tools)** are defined in
> [021-M9-graph-introspection](021-M9-graph-introspection.md).

---

## Alternatives Considered

### A. Exposing `Consumer<T>` from other records in `.source()`

**Idea:** Let `.source()` accept multiple `Consumer<T>` handles for input
records, rather than introducing a new primitive.

**Rejected because:**
- Blurs the semantic distinction between autonomous sources and derived data
- Input dependencies would still be invisible to AimDB
- No type-safe builder pattern for multi-input configuration

### B. Separate `.map()`, `.filter()`, `.accumulate()`, `.join()` methods

**Idea:** One method per transform archetype.

**Rejected because:**
- Combinatorial explosion (stateful map? filtered join? accumulated filter?)
- Over-constrains the API — a single `.transform()` with a builder handles
  all cases through composition
- But: `.transform()` (single-input) and `.transform_join()` (multi-input)
  is an acceptable split because the type signatures are fundamentally different

### C. Channel-based transform (similar to current workaround but first-class)

**Idea:** AimDB provides managed mpsc channels between records.

**Rejected because:**
- Still requires manual wiring
- State management still falls on the user
- No type-safe builder pattern
- Marginal improvement over current workaround

### D. Dataflow DSL (separate from record registration)

**Idea:** A graph DSL that declares transforms separately from records.

**Rejected because:**
- Breaks the per-record registration pattern that makes AimDB ergonomic
- Harder to read — splits related configuration across two locations
- Unnecessary complexity for the common case (1–2 inputs, straightforward logic)

### E. "Fix the bridge" — keep `.source()` + mpsc + `Arc<Mutex<>>` *(added v1.1)*

**Idea:** Don't add `.transform()` at all. Instead, add a diagnostic log to
`validate_forecasts()` and debug the specific bridge wiring bug.

**Rejected because:**
- The [production incident](#incident-analysis--the-bridge-that-failed-silently)
  demonstrated that the bridge pattern has **inherent reliability problems**
  that cannot be solved by debugging one instance:
  - 30 connection points (5 cities × 3 closures × 2 state mechanisms) that
    can fail silently
  - No build-time validation of the wiring
  - No runtime introspection of the data flow
  - A fix for one city doesn't prevent the same class of bug in the next
    feature that needs record-to-record derivation
- The bridge pattern scales poorly: every new derived record requires
  re-inventing the same mpsc + mutex + relay-tap + reactive-source boilerplate
- Fixing the specific bug doesn't eliminate the semantic confusion (`.source()`
  pretending to be reactive, `.tap()` pretending to be a producer)
- A diagnostic log was added as **insurance** (`tracing::info!("🔮 Reactive
  validation .source() started for {city}")`) but this is a band-aid, not a fix

---

## Future Extensions

### Windowed Transforms

After the history/drain API (019-M8), transforms could operate on time windows
rather than individual values:

```rust
.transform(TempKey::Vienna, |ctx| {
    ctx.window(Duration::from_secs(3600))  // 1-hour sliding window
       .on_window(|values: &[Temperature], state| {
           Some(HourlyStats {
               min: values.iter().map(|t| t.celsius).min(),
               max: values.iter().map(|t| t.celsius).max(),
               avg: values.iter().map(|t| t.celsius).sum::<f32>() / values.len() as f32,
           })
       })
})
```

### Built-In Sink Adapters

Common tap patterns (like `ws_tap`) could become built-in:

```rust
reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
   .transform(TempKey::Vienna, |ctx| ctx.map(|t| to_alert(t)))
   .broadcast_to(ws_tx.clone(), |val| val.to_ws_message("vienna"))  // built-in
```

### Record Group Templates

Repeated per-city registration could be abstracted:

```rust
builder.configure_group::<Temperature, _>(
    [TempKey::Vienna, TempKey::Berlin, TempKey::Prague],
    |key, reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
           .tap(move |ctx, c| ws_tap(ctx, c, tx.clone(), key.as_str()))
           .link_from(&key.link_address().unwrap())
           .with_deserializer(Temperature::from_bytes)
           .finish();
    },
);
```

### Transform Metrics

Since transforms are AimDB-managed, built-in metrics are natural:

- **Throughput:** values/sec processed per transform
- **Latency:** input-to-output delay per transform
- **Skip rate:** fraction of inputs that produced `None` (filtered)
- **State size:** estimated memory usage of transform state (if `S: Sized`)

---

## Open Points & Roadblocks

### 1. Type Erasure for Multi-Input Joins

The join builder needs to store subscribers for heterogeneous input types in
a single `Vec`. This requires type-erasing the `BufferReader<T>` into a
`BufferReader<Box<dyn Any>>` or similar. The current `BufferReader` trait is
generic over `T`, so we need either:

- A `JsonBufferReader` approach (serialize to a common format) — loses type safety
- A `DynBufferReader` that wraps `recv()` into `Box<dyn Any>` — slight allocation overhead
- Per-input spawned tasks that forward into a single `mpsc` — simplest, small overhead

**Recommendation:** Per-input forwarding tasks. Each input gets a lightweight
spawned task that calls `reader.recv().await` and sends a `JoinTrigger` into
a shared `mpsc::channel`. The main join task reads from this single channel.
This avoids trait object gymnastics and works with the existing buffer API.

> **v1.1 note:** This approach is structurally identical to the bridge pattern
> that `.transform_join()` replaces — but critically, it is **internal to
> AimDB** rather than user-managed. The forwarder tasks are spawned
> automatically, the mpsc channel is hidden, and the user never sees it.
> The failure modes that affected the Weather Hub bridge (wrong channel
> instance, captured by wrong closure, sender/receiver mismatch) cannot
> occur because all wiring is done by a single code path in
> `run_join_transform`.

### 2. Spawn Ordering

If Record B transforms from Record A, does A's buffer need to exist before B's
transform task subscribes? Yes — but this is already handled by the current
`build()` sequence: all buffers are created before any tasks are spawned.
The topological sort is only needed if we want to guarantee that A's *source*
starts producing before B's transform subscribes, which is generally not
required (the transform simply blocks on `recv()` until data arrives).

### 3. Error Propagation

What happens if a transform's input record is dropped or its buffer closes?
The transform task's `recv()` loop returns `Err`, and the task exits. This
matches the current behavior of taps when their record is dropped. No special
handling needed — but we should log a warning.

> **v1.1 update:** Per [Incident Lesson #3](#lessons-for-transform-implementation),
> **subscription failure** (as opposed to runtime buffer close) must be treated
> as fatal. The distinction:
> - `consumer.subscribe()` returns `Err` → configuration bug → **log + return
>   immediately** (with `eprintln!` fallback if tracing is compiled out)
> - `reader.recv()` returns `Err(BufferClosed)` → graceful shutdown → **log
>   warning and exit**
> - `reader.recv()` returns `Err(BufferLagged)` → slow consumer → **log
>   warning and continue**

### 4. Backpressure Between Transforms

In a chain `A → B → C`, if C's transform is slow, B's output buffer fills up.
This is handled by existing buffer semantics (ring buffer overwrites oldest
values for `SpmcRing`; `SingleLatest` always overwrites). No new backpressure
mechanism is needed, but users should be aware that slow transforms may cause
data loss in ring buffers, just like slow taps do today.

### 5. Graph Serialization Cost

See [021-M9-graph-introspection § Open Points](021-M9-graph-introspection.md#9-open-points).
The graph is immutable after `build()`, so the recommendation is to
pre-serialize once and serve cached representations.

### 6. Future Embassy Considerations

Embassy support is explicitly out of scope for this proposal. When added
later, the key considerations will be:

- Single-input transforms should port directly (no `std` dependencies)
- Multi-input joins will need `embassy_futures::select` instead of
  `tokio::select!`, and compile-time-fixed input counts (2/3/4) to
  avoid dynamic allocation
- Each transform consumes one spawned task from the bounded Embassy
  task pool (8/16/32 via feature flags)

### 7. Silent Failure in Core Spawn Infrastructure *(new in v1.1)*

The production incident revealed that **all** spawn lifecycle logging in
`typed_record.rs` is gated behind `#[cfg(feature = "tracing")]`. Since
most production builds don't enable this feature on `aimdb-core`, the
entire spawn pipeline (sources, taps, and future transforms) executes
without any observable confirmation.

**For transforms specifically**, this is unacceptable because a transform
that fails to start looks identical to a transform that was never registered
— there is no external signal (sensor data, MQTT messages) that would reveal
the absence.

**Recommendation for the transform implementation:**

- Transform startup and subscription-failure logs should use `tracing` when
  available but fall back to `eprintln!` under `#[cfg(feature = "std")]`.
- Consider a follow-up proposal to promote key lifecycle events
  (`spawn_producer_service`, `spawn_consumer_tasks`, `spawn_transform_task`)
  to always-on logging across all three primitives. This would prevent the
  same class of silent failure for `.source()` and `.tap()` as well.
- The `AnyRecord` trait's `has_producer_service()` should be extended with
  `has_transform()` so MCP introspection can distinguish "no producer" from
  "has transform" from "passive record."

### 8. `TransformDescriptor` Needs the `AimDb` Handle *(new in v1.1)*

A transform's `spawn_fn` needs the fully-constructed `AimDb<R>` handle to
subscribe to input buffers via `Consumer::new(db, key)`. However, the
`AimDb` handle doesn't exist at registration time — it's created during
`build()`.

The current `ProducerServiceFn` solves this by receiving the `Producer` and
runtime context at spawn time (the `Producer` internally holds a cloned `db`
handle). For transforms, we need the **same pattern** but additionally need
to create `Consumer` handles for input records.

**Resolution:** The `TransformDescriptor.spawn_fn` captures input keys in its
closure. At spawn time, it receives a `Producer<T, R>` (which already holds
the `db` handle), and creates `Consumer<I, R>` handles by extracting `db`
from the producer's internal reference. Alternatively, the `spawn_fn` can
accept an additional `Arc<AimDb<R>>` parameter — mirroring how the existing
spawn function closures in `builder.rs` line ~488 receive `db` as a parameter.

The cleanest approach is to have `spawn_transform_task()` pass the `db` handle
explicitly, just as `spawn_producer_service()` already receives `db` as a
parameter (see `typed_record.rs` line ~894).

---

## References

- [021-M9-graph-introspection](021-M9-graph-introspection.md) — Companion: AimX/MCP exposure of the dependency graph
- [004-M2-api-refactor-source-tap-link](004-M2-api-refactor-source-tap-link.md) — Original `.source()` / `.tap()` design
- [019-M8-record-history-api](019-M8-record-history-api.md) — Ring buffer history (enables future windowed transforms)
- [013-M6-buffer-introspection-metrics](013-M6-buffer-introspection-metrics.md) — Buffer metrics (extensible to transforms)
- Weather Hub Pro (`aimdb-pro/demo/weather-hub-streaming/src/main.rs`) — Primary motivating example
- Reactive Extensions (Rx) `combineLatest` / `withLatestFrom` operators
- Apache Flink stateful stream processing model
