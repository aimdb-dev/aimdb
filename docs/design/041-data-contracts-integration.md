# 041 — Data-Contracts Integration: make the capability story real (D1/D12)

**Status:** Proposed 2026-07-06. Follow-up design doc for the design-038 **D1/D12** decision (tracking issue [#161](https://github.com/aimdb-dev/aimdb/issues/161)). Companion to the claims-assessment track (039, 040).
**Scope:** `aimdb-data-contracts` (add two registrar extension traits — one of which also gains a build-time sim-to-real source selector), the README capability table, and the weather-mesh example (adopt the new methods). **No changes to the contract trait definitions themselves** — `Simulatable`, `Observable`, `Streamable`, `Linkable`, `Migratable` keep their current shape.
**Goal:** Turn `aimdb-data-contracts` from a *side-channel* (traits nothing consumes) into a *consumed layer*, so the README's "capability traits" story is backed by code the engine actually runs — using the exact mechanism a third party would use.

---

## 1. Problem

Design 038 findings **D1** ("building abstractions for consumers that never arrived") and **D12** ("marketing vocabulary in API design") both landed on the same subject: the data-contract capability traits are advertised as a headline feature, but core consumes none of them, and two of them — `Simulatable` and `Observable` — have **no non-example consumer at all**.

Concretely, today:

- `Simulatable::simulate()` is only ever called **by hand**, inside example producer loops. Each weather station repeats ~30 lines of "make an RNG → loop → `T::simulate(...)` → `produce` → sleep" boilerplate ([`weather-station-beta/src/main.rs:132-189`](../../examples/weather-mesh-demo/weather-station-beta/src/main.rs), [`weather-station-gamma/src/main.rs:80-125`](../../examples/weather-mesh-demo/weather-station-gamma/src/main.rs)).
- `Observable`'s only non-example consumer is the opt-in `log_tap()` helper, which the user must wire manually as a tap ([`weather-hub/src/main.rs:36`](../../examples/weather-mesh-demo/weather-hub/src/main.rs)).
- `RecordRegistrar::source()` / `.tap()` are fully generic with **no trait bounds** — and they must stay that way (they are general-purpose stage installers, not contract-specific).

The 2026-07-01 owner decision (recorded in 038 §4 D1/D12) resolved the tension **by completion, not deletion**: data contracts are core to AimDB's identity, so the fix is to *add the missing consumer*, not remove the API. This doc specifies that consumer.

## 2. The model to copy: `.persist()`

`aimdb-persistence` already demonstrates the pattern this doc generalizes. `RecordRegistrarPersistExt::persist()` ([`aimdb-persistence/src/ext.rs`](../../aimdb-persistence/src/ext.rs)) is an **extension trait on `RecordRegistrar`** that installs a `.tap()` — the engine drives it exactly like any other tap, and `aimdb-core` never learns persistence exists:

```rust
// aimdb-persistence/src/ext.rs (abbreviated)
impl<'a, T> RecordRegistrarPersistExt<'a, T> for RecordRegistrar<'a, T> {
    fn persist(&mut self, record_name: impl Into<String>) -> &mut RecordRegistrar<'a, T> {
        // ... pull backend from Extensions TypeMap ...
        self.tap(move |_ctx, consumer| async move { /* subscribe → serialize → store */ })
    }
}
```

The two properties we copy:

1. **Extension trait on `RecordRegistrar`**, not a bound on `.source()`/`.tap()`. The generic stage installers stay generic; the contract-specific ergonomics live in the contract crate.
2. **The engine consumes the trait through the same mechanism third parties would use** — `.source()`/`.tap()` are public API; a `simulate()` that calls `.source()` is doing nothing a downstream crate couldn't do itself.

**One difference from `.persist()`:** persistence needed builder-side state (a backend registered via `.with_persistence()`, retrieved from the `Extensions` TypeMap). The contract extensions need **no builder-side state** — `simulate(cfg)` receives its config inline and `observe(node_id)` receives its label inline. So there is **no `.with_*()` builder extension** in this design; both methods are pure registrar-time sugar.

## 3. Design

Two extension traits added to `aimdb-data-contracts`, each gated behind the feature that already carries its contract trait.

### 3.1 `SimulatableRegistrarExt::simulate(cfg, rng)` (feature `simulatable`)

Installs the generate → produce → sleep source loop that examples write by hand. **The caller supplies the RNG** (decision §5.1): `Simulatable::simulate<R: rand::Rng>` is generic over the RNG, and no_std/Embassy has no default entropy source, so `.simulate()` takes an owned `R: rand::Rng + Send + 'static` that moves into the source future.

```rust
// aimdb-data-contracts/src/simulatable.rs  (new ext, feature = "simulatable")
pub trait SimulatableRegistrarExt<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    /// Install a source that emits `T::simulate(...)` every `cfg.interval_ms`,
    /// driving the supplied RNG. Works on any runtime — the caller chooses the
    /// RNG (OS entropy on std, a seeded PRNG on no_std).
    fn simulate<R>(&mut self, cfg: SimulationConfig, rng: R) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static;
}

impl<'a, T> SimulatableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    fn simulate<R>(&mut self, cfg: SimulationConfig, mut rng: R) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static,
    {
        if !cfg.enabled {
            // Simulation gated off by config → install no source. The record
            // can still receive a producer elsewhere. (Mirrors `.persist()`'s
            // no-op-when-unconfigured precedent.)
            return self;
        }
        self.source(move |ctx, producer| async move {
            let mut prev: Option<T> = None;
            loop {
                let now_ms = ctx
                    .time()
                    .unix_time()
                    .map(|(s, ns)| s.saturating_mul(1000) + (ns / 1_000_000) as u64)
                    .unwrap_or(0);
                let next = T::simulate(&cfg, prev.as_ref(), &mut rng, now_ms);
                producer.produce(next.clone());
                prev = Some(next);
                ctx.time().sleep_millis(cfg.interval_ms.max(1)).await;
            }
        })
    }
}
```

Notes:
- **Caller-supplied RNG** keeps `.simulate()` runtime-agnostic and drops the hard dependency on `rand`'s `std_rng` feature: std callers pass `StdRng::from_rng(&mut rand::rng())` (what the demo already constructs), an Embassy target passes a seeded PRNG. Deterministic tests get a fixed seed for free.
- Uses `RuntimeContext::time()` for both the timestamp (`unix_time()`) and the delay (`sleep_millis()`) — so the loop is runtime-neutral (works on any adapter), exactly as `.persist()`'s cleanup loop is.
- `.simulate()` installs a **source**, so it participates in the existing writer-exclusivity rule (a simulated record cannot also be a `.transform()`/`.link_from()` source — the conflict is reported from `build()`, unchanged).
- Removes ~30 lines of boilerplate per station and deletes the manual `db.producer::<T>(...)` + `tokio::spawn` dance.

### 3.2 `ObservableRegistrarExt::observe(node_id)` (feature `observable`)

Installs `log_tap` as a tap — the one-liner that replaces the manual `.tap(|ctx, c| log_tap(ctx, c, id))`.

```rust
// aimdb-data-contracts/src/observable.rs  (new ext, feature = "observable")
pub trait ObservableRegistrarExt<'a, T>
where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Install a logging tap that prints `T::format_log(node_id)` per value.
    fn observe(&mut self, node_id: &'static str) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> ObservableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn observe(&mut self, node_id: &'static str) -> &mut RecordRegistrar<'a, T> {
        self.tap(move |ctx, consumer| log_tap(ctx, consumer, node_id))
            .with_name("observe") // surfaces the tap in stage profiling (no-op w/o `observability`)
    }
}
```

Notes:
- `log_tap` is unchanged and stays exported for callers who want the raw form.
- `.with_name("observe")` gives the tap a stable identity in the stage-profiling snapshot when the `observability` feature is on — a small, honest link to the observability subsystem (see §4).

### 3.3 `SimulatableRegistrarExt::source_or_simulate(mode, cfg, rng, real)` (feature `simulatable`)

A record has **exactly one producer**: `set_producer` rejects a second `.source()`, and a source is mutually exclusive with `.transform()`/`.link_from()` — validated once in `build()`, not panicked ([`typed_record.rs:570`](../../aimdb-core/src/typed_record.rs)). Both `.simulate()` (§3.1) and a real `.source()` install *that same slot*. So the sim-to-real workflow — "run against a real sensor in production, against `Simulatable` in dev / CI / demo / when the hardware is absent" — is **not** a coexistence problem. It is a **build-time source selection**: choose which producer fills the one slot. This keeps single-writer-per-key true *by construction* (only one branch ever installs a producer, so `build()` never sees a conflict) and costs nothing at runtime.

The combinator adds a second method to the §3.1 trait, wrapping the branch a caller would otherwise write by hand:

```rust
// aimdb-data-contracts/src/simulatable.rs  (feature = "simulatable")

/// Which producer fills a record's single source slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    /// Install the real `.source()` closure (hardware / network feed).
    Real,
    /// Install the `Simulatable` generate → produce → sleep loop (§3.1).
    Simulated,
}

pub trait SimulatableRegistrarExt<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    // fn simulate<R>(&mut self, cfg: SimulationConfig, rng: R) -> ...  // §3.1

    /// Install `real` when `mode == Real`, else the §3.1 simulation loop.
    /// Exactly one producer is installed either way, so writer-exclusivity
    /// holds and `build()` never reports a conflict.
    fn source_or_simulate<F, Fut, R>(
        &mut self,
        mode: SourceMode,
        cfg: SimulationConfig,
        rng: R,
        real: F,
    ) -> &mut RecordRegistrar<'a, T>
    where
        F: FnOnce(RuntimeContext, Producer<T>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
        R: rand::Rng + Send + 'static;
}

impl<'a, T> SimulatableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    fn source_or_simulate<F, Fut, R>(
        &mut self,
        mode: SourceMode,
        cfg: SimulationConfig,
        rng: R,
        real: F,
    ) -> &mut RecordRegistrar<'a, T>
    where
        F: FnOnce(RuntimeContext, Producer<T>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
        R: rand::Rng + Send + 'static,
    {
        match mode {
            SourceMode::Real => self.source(real),          // the one general-purpose stage installer
            SourceMode::Simulated => self.simulate(cfg, rng), // §3.1
        }
    }
}
```

Notes:
- **`mode` is caller-derived, exactly like `rng` (§5.1).** The ext stays runtime-agnostic — it does not read env or entropy itself. A std station derives it from an env var / config field (`AIMDB_SOURCE=sim|real`); a no_std target selects with a **Cargo feature** so the real-hardware source only *compiles* on the board (`#[cfg(feature = "hw")]`) and simulation is the fall-through. The record wiring is identical both ways — this is the embedded "develop against sim, flash against silicon" story made first-class, not a demo detail.
- **`cfg.enabled` stays sim's internal gate, not the selector.** `mode` picks the branch; within `Simulated`, `enabled: false` still installs no source (§3.1). `Simulated` + `enabled: false` is therefore the explicit "leave this record writer-less" case — identical to calling `.simulate()` with a disabled config today.
- **Both `real` and `rng` are owned and moved into the chosen branch;** the unused one is dropped at registration. No boxing of the unused arm, no runtime flag — the selection is fully resolved before `build()`.
- **`SourceMode` is re-exported next to `SimulationConfig`/`SimulationParams`** from `aimdb-data-contracts` (lib.rs `pub use`).
- **Runtime failover is out of scope.** Falling over to simulated values *live* when a sensor drops, then back, cannot be two sources (that breaks writer-exclusivity); it needs a single source future that internally multiplexes real vs. `T::simulate(...)`. Recorded as a follow-up in §4 / §7.

### 3.4 What stays out

- **No trait bounds added to `.source()`/`.tap()`/`.transform()`.** They remain general-purpose.
- **No new dependency in `aimdb-core`.** The direction of the dependency is `aimdb-data-contracts → aimdb-core`, never the reverse — same as persistence.
- **No change to the five contract trait definitions.** Per the issue and the user's steer: keep the traits as they are; only add the integration layer.
- **No runtime sim↔real hot-swap.** `source_or_simulate` selects once, at registration; a live failover multiplexer is a separate design (§7).

## 4. Every capability trait: current state → target

The issue asks for the `Observable` claim to be made honest. Rather than fix that row in isolation, we set a **current state → target** entry for *every* capability trait — this is the map the README table and future integration work should track against. The through-line: a trait's "target" is *the engine consuming it*, and the README should describe the **current** state, not the target.

| Trait | Current state (verified) | Target |
|---|---|---|
| `Streamable` | **Consumed.** Bound by `aimdb-websocket-connector`'s `StreamableRegistry::register<T: Streamable>()` ([`server/registry.rs:33`](../../aimdb-websocket-connector/src/server/registry.rs), `server/builder.rs:250`) as the schema-name gate for browser streaming. Marker trait (no methods). | Keep as the "may cross a serialization boundary" marker + registry key. Already real — README row is accurate. |
| `Migratable` | **Consumed.** `migration_chain!` drives runtime migration; used by benches (`b0_alloc_migration`) and the weather example. Works `no_std` (design 039). | Keep. Stretch: auto-apply the chain on the deserialize/`link_from` path so connectors migrate old wire data without a manual call. |
| `Observable` | **Barely consumed.** Signal extraction + `format_log()` only; sole consumer is the opt-in `log_tap()`, wired by hand. **Not** connected to metrics. | `.observe(node_id)` installs the log tap (§3.2). README reworded (below). Statistics come from the separate `observability` feature, not this trait. |
| `Linkable` | **Convention only.** No engine bound anywhere — examples call `T::to_bytes()`/`from_bytes()` by hand inside `with_serializer`/`with_deserializer` closures. | `.link_from`/`.link_to` default their (de)serializer to `Linkable::{from_bytes,to_bytes}` when `T: Linkable`, so the connector consumes the trait directly. *(Own follow-up — not in this doc's PR.)* |
| `Simulatable` | **Unconsumed.** Called by hand in example producer loops only. | `.simulate(cfg, rng)` installs the source loop (§3.1); `.source_or_simulate(mode, …)` picks real-vs-sim for the single producer slot (§3.3). *Stretch:* runtime sim↔real failover (live switch on sensor staleness) — its own doc. |
| `Settable` | **Unconsumed.** `set(value, ts)`; examples `impl Settable`, nothing calls it outside codegen templates. | Back a typed `.set(value)` producer/one-shot helper. *(Own follow-up — noted for completeness; not in scope here.)* |

This doc delivers the `Observable` and `Simulatable` rows. `Linkable`/`Settable` targets are recorded so the map is complete, but are separate follow-ups (each needs its own caller-in-the-same-PR).

### 4.1 The `Observable` README claim — reword (decided)

The capability table ([`README.md:137`](../../README.md)) currently reads `Observable | Automatic per-record metrics`. This is wrong on two counts:

1. **`Observable` is about log formatting, not metrics.** Its surface is `signal()`, `ICON`, `UNIT`, `format_log()` ([`aimdb-data-contracts/src/lib.rs:192-227`](../../aimdb-data-contracts/src/lib.rs)) — domain-signal extraction + human-readable output.
2. **The per-record metrics that *do* exist are independent of `Observable`.** `BufferMetricsSnapshot` (produced/consumed/dropped/occupancy) is collected by the buffer for **every** record under the `observability` feature ([`aimdb-core/src/buffer/traits.rs:278`](../../aimdb-core/src/buffer/traits.rs)), regardless of whether the type is `Observable`.

**Decision (2026-07-06): reword to match reality.** `Observable` lets the user *tap and log* the record; the `observability` **feature flag** is what provides statistics — two separate things the README wrongly fused. New row:

| Trait | What it unlocks | Runtimes |
|---|---|---|
| `Observable` | Built-in logging tap (`.observe()`) over an extracted signal | std, no_std |

The README's separate feature-flag section should carry the "per-record buffer statistics" story where it belongs (`observability`). `.observe()`'s `.with_name("observe")` (§3.2) still gives the tap a real identity in the stage-profiling snapshot — an honest, modest link, no overclaim. A signal-metrics sink (min/max/mean of `signal()` into the snapshot) remains possible later but is out of scope and would get its own design doc.

## 5. Decisions (resolved 2026-07-06)

### 5.1 RNG source for `.simulate()` — **caller-supplied**

`Simulatable::simulate<R: rand::Rng>` is generic over the RNG; `.simulate()` must *pick* one, and no_std/Embassy has no default entropy source. **Decision: the caller supplies the RNG** — `.simulate(cfg, rng)` takes an owned `R: rand::Rng + Send + 'static` (§3.1). std callers pass `StdRng::from_rng(&mut rand::rng())` (already in the demo); embedded callers pass a seeded PRNG; tests pass a fixed seed. This keeps the method a single signature that works on every runtime and drops the need for `rand`'s `std_rng` feature in the ext path.

### 5.2 `Observable` README claim — **reword** (see §4.1)

Reworded to "built-in logging tap over an extracted signal"; statistics stay attributed to the `observability` feature. Signal-metrics sink deferred to a possible future doc.

### 5.3 New coupling: `simulatable` now pulls `aimdb-core`

`observable` already depends on `aimdb-core` (for `log_tap`). Adding `SimulatableRegistrarExt` means **`simulatable` gains the same `aimdb-core` dependency** (for `RecordRegistrar`/`Producer`/`RuntimeContext`). This is the intended shape of the D1/D12 fix — "turns `aimdb-data-contracts` into a consumed layer" — but it means selecting `simulatable` on a flash-constrained target now compiles core's `alloc` surface. `aimdb-core` is pulled with `default-features = false, features = ["alloc"]` (matching the existing `observable` wiring), so `serde_json`/AimX stay out. If someone ever needs the bare trait + `SimulationConfig` without core, split via a `simulatable-integration` sub-feature — not proposed now (no such caller).

### 5.4 Sim-to-real is build-time selection, not runtime coexistence — **decided**

"Real feed in production, `Simulatable` in dev / CI / demo / absent hardware" is delivered as `source_or_simulate(mode, cfg, rng, real)` (§3.3): a registrar-time branch over the single producer slot. **Rationale:** a record has exactly one producer ([`typed_record.rs:570`](../../aimdb-core/src/typed_record.rs)), so sim and real can never both write — attempting to install both is a writer-exclusivity violation `build()` already rejects, and would contradict single-writer-per-key. Selecting one at registration is zero-cost and invariant-safe, and it reuses the general-purpose `.source()` installer unchanged (no new stage type). **Live** failover (sim↔real at runtime, e.g. on sensor staleness) would instead require a *single* source future that multiplexes real vs. `T::simulate(...)` internally; it is deferred to its own doc (§7), not folded into this selector.

## 6. Migration & examples

- **weather-station-beta / gamma:** replace the manual producer loops with `reg.simulate(cfg, rng)` inside `configure::<T>()`, passing the `StdRng` they already build. Deletes the `tokio::spawn`, the `db.producer::<T>()` handles, and the prev-value bookkeeping.
- **weather-hub:** replace `.tap(move |ctx, c| log_tap(ctx, c, key.as_str()))` with `.observe(node_id)`.
- **`source_or_simulate` caller:** the demo stations are simulation-only (they publish synthetic data; there is no real sensor to select), so the combinator's in-PR caller is an `aimdb-data-contracts` integration test — a fixed-seed RNG plus a stub real source, asserting each `SourceMode` installs the expected single producer and that `build()` reports **no** writer conflict. This satisfies the D1 caller-in-PR rule without fabricating hardware; a station that toggles a *genuine* feed to sim is future work (§7).
- These examples/tests become the **caller** that proves the API — satisfying the D1 rule ("no public API without a caller in the same PR").
- `CHANGELOG.md`: entry under `aimdb-data-contracts` ("added `SimulatableRegistrarExt` (`simulate` + `source_or_simulate`) / `ObservableRegistrarExt`") and README.

## 7. Sequencing

1. Land `ObservableRegistrarExt::observe()` + the README reword (self-contained).
2. Land `SimulatableRegistrarExt::simulate(cfg, rng)`.
3. Land `SimulatableRegistrarExt::source_or_simulate(mode, cfg, rng, real)` on the same trait, with its integration-test caller (§3.3, §6).
4. Convert the weather-mesh examples to the new methods in the same commit as the API they exercise (the caller requirement).
5. Update the README capability table with the current→target-accurate rows (§4).

Per [[feedback_no_stacked_prs_for_design_docs]]: land as separate commits on one branch, one per work item — not a stacked-PR chain.

**Out of scope / recorded for later** (each needs its own caller-in-PR): the `Linkable` and `Settable` targets from §4; the **runtime sim↔real failover** multiplexer (§3.3 note) — a single source future that switches live between the real feed and `T::simulate(...)` on sensor staleness, kept single-writer-safe.
