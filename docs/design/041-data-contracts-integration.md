# 041 — Data contracts as first-class capabilities

**Status:** ✅ Implemented. Follow-up to design-038 **D1/D12** (tracking issue [#161](https://github.com/aimdb-dev/aimdb/issues/161)).

**Scope:** `aimdb-data-contracts` (trait reshapes + three registrar ext traits; `Simulatable` stays here behind its feature), `aimdb-sync` (consumes `Settable`), one small `aimdb-core` observability surface (signal gauges), the README capability table, the weather-mesh example, and a CI guard.

**Goal:** every capability trait is a promise the engine keeps — implemented ⇒ one specific verb becomes available, and that verb is consumed by real code in the same PR (D1 rule).

---

## 1. Problem

Design 038 D1/D12: the capability traits were advertised as a headline feature, but core consumed almost none of them. Three specific gaps:

1. **Simulation was a runtime value, not a compile-time fact.** `SimulationConfig.enabled` gated simulation with a runtime flag, so the sim loop, every `T::simulate` impl, `SimulationConfig`, and `rand` all shipped in the production image even with simulation "off". A build-time *value* selector (e.g. an env-var-driven mode enum) fails the same test: both arms still compile into the binary. The requirement is stronger — production binaries must contain **zero** simulation code, verifiable from the dependency graph.
2. **`Observable` was logging.** The README claimed "automatic per-record metrics", but the trait only powered a console tap (`format_log`). Rewording the claim downward would fix the dishonesty; making the trait actually feed metrics fixes the feature.
3. **`Streamable`/`Linkable`/`Settable` were unintuitive** because a user could not answer *"what does implementing this let me write?"* `Linkable` existed but every example still hand-wrote `with_deserializer` closures; `Settable` was built for `aimdb-sync` but sync never referenced it (`SyncProducer::set` took a full `T`); `Streamable` worked but nothing said its verb is the ws-connector's `.register::<T>()`.

## 2. Organizing principle: one verb per contract, tiered by deployment role

A capability trait is a compile-time promise about a schema type. Each promise unlocks **exactly one verb**, and each contract belongs to a **tier** that states whether it may exist in a production binary:

| Contract | Implement when… | Verb it unlocks | Consumer | Tier |
|---|---|---|---|---|
| `SchemaType` | always (identity) | `configure::<T>(key, …)` | core | identity |
| `Linkable` | the type crosses a per-URL byte boundary (MQTT/KNX/serial/UDS) | `.linked_from(url)` / `.linked_to(url)` (§3.3) | connectors | wire (prod) |
| `Streamable` | the type streams as schema-named JSON (browser/WASM) | ws-connector `.register::<T>()` | [`server/registry.rs:33`](../../aimdb-websocket-connector/src/server/registry.rs) | wire (prod) |
| `Migratable` | the schema evolved across versions | `migration_chain!` | core runtime migration (design 039) | wire (prod) |
| `Settable` | callers outside the AimDB thread set the record from a primitive | `SyncProducer::set_value(v)` (§3.4) | `aimdb-sync` | wire (prod) |
| `Observable` | the type carries a domain signal worth watching | `.observe()` → live signal metrics (§3.2) | introspection surface | introspection (prod, optional) |
| `Simulatable` | the type can generate realistic synthetic data | `.simulate(profile, rng)` (§3.1) | `simulatable` feature ext | **dev-only — never in prod** |

Mechanism (the `.persist()` precedent — [`aimdb-persistence/src/ext.rs`](../../aimdb-persistence/src/ext.rs)): extension traits over `RecordRegistrar`/`SyncProducer` in the crate that owns the contract, installing plain `.source()`/`.tap()`/`.link_*()` stages. `.source()`/`.tap()` stay unbounded; `aimdb-core` never learns the contracts exist; dependency direction is always *contracts → core*, never the reverse.

The tier column is the second half of the fix: *wire* contracts ship in production, *introspection* is prod-optional, and the *dev* tier must be **structurally excludable** — which drives §3.1's compile-time-only design.

## 3. Design

### 3.1 `Simulatable` → compile-time only, behind the `simulatable` feature

#### 3.1.1 Stays in `aimdb-data-contracts`, as the dev-tier feature

`Simulatable` lives in the contracts crate behind the `simulatable` feature. A separate `aimdb-simulation` crate was considered for the structural guarantee and rejected — another crate in the monorepo buys nothing, because the guarantee never depended on a crate boundary. It rests on three enforceable properties:

1. **`simulatable` is not (and never becomes) a default feature** ([`Cargo.toml`](../../aimdb-data-contracts/Cargo.toml)); CI asserts it stays that way (§3.1.5).
2. **Production binaries resolve features per-package.** The workspace uses `resolver = "2"` ([`Cargo.toml:45`](../../Cargo.toml)), so `cargo build -p <prod-bin> --release` unifies features only over that binary's own graph — a sim-enabled example elsewhere in the workspace cannot leak the feature into the release artifact. Corollary, worth one sentence in CONTRIBUTING: release artifacts are built per-package, never lifted out of a `--workspace` build (where unification does cross targets).
3. **`rand` is the tracer.** `Simulatable::simulate<R: rand::Rng>` makes `rand` reachable iff the feature is on, so an inverse-dependency check on `rand` proves sim absence from a production graph (§3.1.5).

Feature wiring: `simulatable = ["rand", "aimdb-core"]` — the ext trait needs `RecordRegistrar`/`Producer`/`RuntimeContext`, the same core coupling `observable` has and `linkable` gains in §3.3. `rand` is `default-features = false` without `std_rng`: the RNG is always caller-supplied (§5.2). The feature is `no_std`-compatible: the "develop against sim, flash against silicon" story is embedded-first.

#### 3.1.2 Trait reshape: domain params, no runtime gate

The previous `SimulationParams` (base/variation/trend/step) was a random-walk config pretending to be universal, and `SimulationConfig.enabled` was a runtime gate that contradicts the compile-time stance. Their replacement:

```rust
// aimdb-data-contracts/src/simulatable.rs  (feature = "simulatable")

/// Dev-only capability: generate realistic synthetic data.
pub trait Simulatable: SchemaType {
    /// Type-specific generation parameters (a temperature defines walk bounds,
    /// a GPS track defines waypoints, …).
    type Params: Clone + Send + Sync + Default + 'static;

    /// Generate the next sample. `previous` enables walks/trends;
    /// `timestamp_ms` is Unix millis supplied by the driving loop.
    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self;
}

/// Loop policy + generation params for one record.
#[derive(Clone, Debug, Default)]
pub struct SimProfile<P> {
    /// Interval between samples (milliseconds).
    pub interval_ms: u64,
    pub params: P,
}

/// Off-the-shelf `Params` for scalar random walks. Impls written against the
/// old `SimulationParams` migrate by setting `type Params = RandomWalkParams`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RandomWalkParams {
    pub base: f64,
    pub variation: f64,
    pub trend: f64,
    pub step: f64,
}
```

There is **no `enabled` field**. "Sim but off" is not a state: production excludes the feature (§3.1.4), and a std dev binary that wants sim conditionally simply branches before calling `.simulate()`.

#### 3.1.3 `SimulatableRegistrarExt::simulate(profile, rng)`

The verb installs a plain `.source()` loop over the reshaped types:

```rust
pub trait SimulatableRegistrarExt<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    /// Install a source that emits `T::simulate(...)` every `interval_ms`,
    /// driving the caller-supplied RNG (OS entropy on std, seeded PRNG on
    /// no_std, fixed seed in tests).
    fn simulate<R>(&mut self, profile: SimProfile<T::Params>, rng: R) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static;
}

impl<'a, T> SimulatableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Simulatable + Send + Sync + Clone + 'static,
{
    fn simulate<R>(&mut self, profile: SimProfile<T::Params>, mut rng: R) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static,
    {
        self.source(move |ctx, producer| async move {
            let mut prev: Option<T> = None;
            loop {
                let now_ms = ctx
                    .time()
                    .unix_time()
                    .map(|(s, ns)| s.saturating_mul(1000) + (ns / 1_000_000) as u64)
                    .unwrap_or(0);
                let next = T::simulate(&profile.params, prev.as_ref(), &mut rng, now_ms);
                producer.produce(next.clone());
                prev = Some(next);
                ctx.time().sleep_millis(profile.interval_ms.max(1)).await;
            }
        })
        .with_name("simulate")
    }
}
```

Runtime-neutral (`ctx.time()` for clock and delay, like `.persist()`'s cleanup loop), installs a **source** so writer-exclusivity is enforced by `build()` unchanged ([`typed_record.rs:570`](../../aimdb-core/src/typed_record.rs)).

#### 3.1.4 Sim-to-real is a `#[cfg]`, not an API

There is deliberately no selector API — no mode enum, no `source_or_simulate(mode, …)` wrapper. Any selector that takes the choice as a *value* (even one read from the environment at build time) compiles both arms into the binary, which is exactly what the dev tier forbids. The sim-to-real selection is the app's Cargo feature, and that pattern is canonical rather than wrapped:

```toml
# app Cargo.toml
[features]
sim = ["aimdb-data-contracts/simulatable", "weather-mesh-common/sim"]
```

```rust
// app wiring — the record configuration is identical either way
builder.configure::<Temperature>(KEY, |reg| {
    #[cfg(feature = "sim")]
    reg.simulate(profile, rng);
    #[cfg(not(feature = "sim"))]
    reg.source(read_hardware);

    reg.buffer_cfg(BufferCfg::SingleLatest);
});
```

The app's contracts crate gates its impls the same way (`#[cfg(feature = "sim")] impl Simulatable for Temperature { … }`). With `sim` off there is nothing to audit: no impls, no `SimProfile`, no `rand` anywhere in the binary's graph. Exactly one producer is installed on either path, so single-writer-per-key holds by construction, with the selection resolved by the compiler instead of a value.

#### 3.1.5 CI guard: prove production is sim-free

A CI step (Makefile target `check-no-sim`) builds the production configuration of each example that has a `sim` feature and asserts the exclusion from the resolved graph — `cargo tree -p <bin> -e normal -i rand` must report no matching package when `sim` is off (`rand` is the tracer, §3.1.1) — and additionally asserts that `simulatable` never appears in `aimdb-data-contracts`' default features. This turns "never ships in production" from a convention into a gate.

### 3.2 `Observable` → the signal, not the log line

#### 3.2.1 Trait reshape: keep the kernel, drop the cosmetics

The trait's valuable kernel is the numeric projection (`Signal`, `signal()`, `UNIT`); `ICON` and `format_log()` were presentation and made the whole trait read as a log formatter. The kernel-only trait:

```rust
pub trait Observable: SchemaType {
    /// Numeric type of the domain signal.
    type Signal: PartialOrd + Copy;

    /// What the signal means, for metrics/UI labels (e.g. "celsius").
    /// Defaults to the schema name.
    const SIGNAL: &'static str = Self::NAME;

    /// Unit label (e.g. "°C", "%", "hPa").
    const UNIT: &'static str = "";

    /// Extract the signal from this instance.
    fn signal(&self) -> Self::Signal;
}
```

`ICON` and `format_log()` are gone: the logging tap (§3.2.3) formats from `Debug` + `UNIT`, and demo-specific decoration (emoji) belongs to the demo.

#### 3.2.2 `.observe()` installs a signal-metrics tap

`.observe()` folds `signal()` into per-record **signal stats** (last/min/max/count/mean as `f64`) that surface on the existing introspection paths. Mechanism — a small, feature-honest core addition mirroring `with_name`:

- `aimdb-core` provides `RecordRegistrar::signal_gauge(name: &'static str, unit: &'static str) -> SignalGaugeHandle`. Under the `observability` feature it registers atomic `SignalStats` on the record's profiling state and returns a live handle; without the feature it is still callable but returns an inert handle (the `with_name` precedent — callers never `cfg` on core's features).
- Exposure: `RecordMetadata` gains an optional signal-stats field, and the AimX `record.get` response carries it — so the signal shows up in `record.list`/`record.get`, the MCP tools built on them, and stage-profiling output. *Implement `Observable`, call `.observe()`, and your domain signal is live on every introspection surface.* That is the README claim, and the gauge is what makes it true.

```rust
// aimdb-data-contracts/src/observable.rs (feature = "observable")
pub trait ObservableRegistrarExt<'a, T>
where
    T: Observable + Send + Sync + Clone + 'static,
    T::Signal: Into<f64>,
{
    /// Feed `T::signal()` into the record's signal gauge (last/min/max/mean),
    /// visible via record.list / record.get / stage profiling.
    fn observe(&mut self) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> ObservableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Observable + Send + Sync + Clone + 'static,
    T::Signal: Into<f64>,
{
    fn observe(&mut self) -> &mut RecordRegistrar<'a, T> {
        let gauge = self.signal_gauge(T::SIGNAL, T::UNIT);
        self.tap(move |_ctx, consumer| async move {
            let mut reader = consumer.subscribe();
            while let Ok(value) = reader.recv().await {
                gauge.update(value.signal().into());
            }
        })
        .with_name("observe")
    }
}
```

Notes:
- `T::Signal: Into<f64>` is bounded **at the consumption site**, not in the trait — `f32`/`i32`/`u32` signals qualify; a type with an exotic signal can still implement `Observable` and write its own tap.
- `.observe()` takes **no `node_id`** — the record key already identifies the gauge; `node_id` was a logging concern.

#### 3.2.3 Logging stays, honestly named

`log_tap` remains for humans watching a console, exposed as `.log(node_id)` on the same ext trait (formats `[node_id] key: {signal}{UNIT} {value:?}` from `Debug` — no trait items needed). The README row for `Observable` says metrics; logging is a bullet under it, not the definition.

### 3.3 `Linkable` → the default codec that `.link_*` actually consumes

Before this design, implementing `Linkable` changed nothing — every example hand-wired `with_deserializer`/`with_serializer` closures. The fix: one-line link verbs, ext trait in the contracts crate (dependency direction preserved):

```rust
// aimdb-data-contracts/src/linkable.rs (feature = "linkable")
pub trait LinkableRegistrarExt<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// `.link_from(url)` with the codec defaulted to `T::from_bytes`.
    fn linked_from(&mut self, url: &str) -> &mut RecordRegistrar<'a, T>;
    /// `.link_to(url)` with the codec defaulted to `T::to_bytes`.
    fn linked_to(&mut self, url: &str) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> LinkableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn linked_from(&mut self, url: &str) -> &mut RecordRegistrar<'a, T> {
        self.link_from(url)
            .with_deserializer(|_ctx, bytes| T::from_bytes(bytes))
            .finish()
    }

    fn linked_to(&mut self, url: &str) -> &mut RecordRegistrar<'a, T> {
        self.link_to(url)
            .with_serializer(|_ctx, value| {
                value.to_bytes().map_err(|_| SerializeError::InvalidData)
            })
            .finish()
    }
}
```

Notes:
- **Inbound matches exactly** (`with_deserializer` takes `Result<T, String>`). **Outbound needs one lossy mapping**: `with_serializer` returns `Result<Vec<u8>, SerializeError>`, so the `String` detail is dropped to `SerializeError::InvalidData`. Issue #177 reuses that existing connector error for its additive `encode_into` method without changing either legacy signature. Replacing `String` across `Linkable` and the connector deserializer remains separate breaking follow-up scope (§7).
- **The raw builders remain the escape hatch** for per-link options (`with_config`, QoS ext traits, topic providers/resolvers). `.linked_from`/`.linked_to` are the 80% path.
- **JSON boilerplate gets a derive:** `#[derive(Linkable)]` in `aimdb-derive` emitting `serde_json::to_vec`/`from_slice` (JSON is the default format; binary formats implement by hand, as the KNX DPT codecs rightly do). Per the D1 rule, the derive replaces a hand-written JSON impl in the same change. After #155, this convenience lives behind `linkable-json`; base `linkable` stays format-neutral and JSON-free for Postcard/custom codecs.
- **Coupling:** the `linkable` feature gains an `aimdb-core` dependency for the ext trait (`default-features = false, features = ["alloc"]`, same wiring as `observable`). `linkable-json` adds the optional `serde_json` + derive dependencies.
- **Three serialization stories, stated once** so users stop guessing:

  | Boundary | Mechanism | Contract |
  |---|---|---|
  | Schema-named JSON streaming (browser/WASM) | ws registry keyed by `SchemaType::NAME` | `Streamable` |
  | Per-URL opaque payloads (MQTT/KNX/serial/UDS) | fused per-link codec | `Linkable` |
  | AimX `record.get`/`record.set`/`subscribe` JSON | blanket `RemoteSerialize` ([`codec.rs:33`](../../aimdb-core/src/codec.rs)), automatic for serde types | none needed |

#### 3.3.1 Issue #177 follow-up: bounded into-slice encoding

Issue #177 implements the non-allocating outbound half without forcing the
deferred breaking error migration. `Linkable` now has a source-compatible
`encode_into(&mut [u8]) -> Result<usize, SerializeError>` default and an
`ENCODE_BUFFER_CAPACITY` opt-in. Existing implementations stay on `to_bytes`;
generated Postcard implementations opt in and call `postcard::to_slice`.

The ownership boundary is deliberately in the route pump:

```text
route pump starts once
  scratch: Vec<u8> with fixed length N

for each T
  FusedReader::recv_into(&mut scratch)
    success(len <= N) -> SerializedPayload::Scratch { len }
    BufferTooSmall   -> old to_bytes() -> SerializedPayload::Owned(Vec)
    InvalidData      -> log and skip
  pump validates scratch.get(..len)
  Connector::publish(&scratch[..len]).await
  only then may the next record overwrite scratch
```

Returning a slice borrowed from reader-owned storage was rejected: a boxed
async retry loop cannot safely lend that slice and then mutably reuse the same
storage on a later iteration. Returning only `{ len }` metadata lets the pump
borrow its own storage across `publish().await`, needs no `unsafe`, and keeps the
additive `SerializedReader::recv_into` default source-compatible for connector
authors.

The compatibility-first error decision is explicit:

- `SerializeError::{BufferTooSmall, InvalidData}` is reused by both the existing
  owned serializer and the new into-slice encoder. No new codec error type is
  introduced.
- Root `aimdb_core::CodecError` still belongs to session-envelope codecs and is
  intentionally not reused.
- `Linkable::from_bytes`/`to_bytes` still return `String`. Migrating those
  signatures remains separate breaking follow-up scope.

The allocation claim is correspondingly narrow: successful generated Postcard
encoding performs zero heap allocations, measured over 10,000 calls. The
existing boxed `SerializedReader` future, dynamic topic `String`, and
connector-internal ownership copies are outside that claim.

### 3.4 `Settable` → consumed by `aimdb-sync`

`Settable` was built for the sync bridge, but `aimdb-sync` never referenced it: `SyncProducer::set(value: T)` took a fully constructed record, so every outside-the-thread caller hand-assembled the struct. The verb the trait was named for is a small inherent impl — and note it is distinct from AimX's existing `record.set {name, value}` (full JSON value through `JsonCodec`); `Settable` is **set-by-primitive**:

```rust
// aimdb-sync/src/producer.rs (inherent impl, feature = "data-contracts")
impl<T> SyncProducer<T>
where
    T: aimdb_data_contracts::Settable + Send + 'static + Debug + Clone,
{
    /// Construct via `T::set(value, now)` and send. Blocking, like `set()`.
    pub fn set_value(&self, value: T::Value) -> SyncResult<()> {
        self.set(T::set(value, unix_now_ms()))
    }

    /// Non-blocking variant, like `try_set()`.
    pub fn try_set_value(&self, value: T::Value) -> SyncResult<()> {
        self.try_set(T::set(value, unix_now_ms()))
    }

    /// Explicit-timestamp variant (replay, testing).
    pub fn set_value_at(&self, value: T::Value, timestamp_ms: u64) -> SyncResult<()> {
        self.set(T::set(value, timestamp_ms))
    }
}
```

Notes:
- **Dependency direction:** `aimdb-sync` gains `aimdb-data-contracts = { optional = true, default-features = false, features = ["settable"] }` behind a `data-contracts` feature. The contracts crate does **not** grow a `sync` feature.
- **Clock semantics (documented, one sentence):** `set_value` stamps with the *caller's* `SystemTime` (sample time at the edge), not the engine's `ctx.time()`; `set_value_at` exists for callers that need control. `aimdb-sync` is std-only, so `SystemTime` is always available.
- **Single-writer untouched:** `set_value` flows through the same channel as `set()`; the sync bridge remains the record's one producer, and concurrent `SyncProducer` clones already arbitrate through that one request stream.
- **Feature gate:** `Settable` sits behind a `settable` feature for tier symmetry with the other wire contracts (codegen templates that emit `impl Settable` enable it).
- A future AimX **set-by-primitive** verb (`aimdb set temperature 22.5` from CLI/MCP) becomes a second consumer of the same trait — recorded in §7, not in scope. The read/write symmetry is explicit: `Observable::signal()` is the generic *read* projection, `Settable::set()` the generic *write* injection.

### 3.5 `Streamable` — unchanged

Already consumed (ws-connector registry bound). Its fix is the verb table (§2) and the serialization-stories table (§3.3): the README states its verb is `.register::<T>()` on the ws connector builder. No trait change.

### 3.6 `Migratable` — unchanged

Consumed since design 039, works no_std. Stretch (recorded, not in scope): auto-apply the migration chain on the `linked_from` deserialize path so connectors migrate old wire data without a manual call.

## 4. Crate & feature layout

```
aimdb-data-contracts
  features: linkable (+core), linkable-json (+serde_json, derive),
            observable (+core), migratable, settable,
            simulatable (+core, rand nf — no std_rng)   # dev tier — never a default feature
  (no new crate: aimdb-simulation split considered and rejected, §3.1.1)

aimdb-sync
  features: data-contracts → aimdb-data-contracts (nf, settable)

aimdb-core
  observability: + SignalStats / RecordRegistrar::signal_gauge (inert handle when off)
  RecordMetadata: + optional signal stats; AimX record.get carries them
```

`aimdb-data-contracts` bumps 0.2.0 → 0.3.0 (breaking: `Simulatable` reshaped — `type Params`, `SimulationConfig`/`SimulationParams` replaced by `SimProfile`/`RandomWalkParams`; `Observable` loses `ICON`/`format_log`; `Settable` gains a feature gate).

## 5. Decisions

1. **Simulation is compile-time.** No runtime `enabled` flag and no selector API (a `source_or_simulate(mode, …)` wrapper was considered and rejected — both arms compile into the binary); the `#[cfg]` branch is the canonical sim-to-real pattern, enforced by the CI guard (`rand` tracer + never-a-default-feature assert). `Simulatable` **stays in `aimdb-data-contracts`** behind `simulatable` — a separate `aimdb-simulation` crate was rejected to keep the monorepo lean; the guarantee rests on §3.1.1's three properties, not on a crate boundary.
2. **The RNG is caller-supplied**; `rand` loses `std_rng` everywhere.
3. **`Simulatable::Params` is an associated type**; `RandomWalkParams` ships as the migration path for existing scalar walks.
4. **`Observable` claims metrics because it delivers metrics**: kernel-only trait, `.observe()` → core signal gauges surfaced via `record.list`/`record.get`/profiling.
5. **`Linkable` gets its verbs in the same design**: `.linked_from`/`.linked_to` + `#[derive(Linkable)]`; the legacy `String` → `SerializeError::InvalidData` mapping remains accepted. Issue #177 reuses `SerializeError` for `encode_into`, while any breaking migration of the legacy `String` signatures stays deferred (§7).
6. **`Settable` is wired to its original purpose**: `SyncProducer::set_value` family in `aimdb-sync`, caller-side clock, `settable` feature gate.
7. **No trait bounds on `.source()`/`.tap()`/`.transform()`; no core→contracts dependency.**

## 6. Migration & examples (the D1 callers)

- **weather-station-beta/gamma:** the manual RNG-loop producers are deleted; `reg.simulate(SimProfile { interval_ms, params: RandomWalkParams { … } }, StdRng::…)` under the app's `sim` feature; contracts impls use `type Params = RandomWalkParams` and `#[cfg(feature = "sim")]`. The stations are the §3.1.4 pattern's reference implementation (with the CI guard building their prod configuration).
- **weather-hub:** `.observe()` replaces the manual `log_tap` tap; one `.log(node_id)` stays where console output is the point of the demo. The hub verifies the signal appears in `record.list`/`record.get`.
- **one connector demo (tokio-mqtt):** `#[derive(Linkable)]` + `.linked_from`/`.linked_to` replace the hand-written closures.
- **aimdb-sync doctest/integration test:** `producer.set_value(22.5)` end-to-end (construct → produce → consume), plus `set_value_at` determinism in a unit test.
- **README:** the capability table is the §2 verb table (Contract / Implement when / Verb / Tier); the `observability` feature keeps the buffer-statistics story; the `simulatable` feature gets a "dev tier — never ships" call-out with the `#[cfg]` pattern.
- **CHANGELOG:** entries for all four crates touched.

## 7. Work items

One branch/PR; each work item lands as its own commit carrying its caller:

1. `Simulatable` reshape (trait + `SimProfile`/`RandomWalkParams` + `.simulate()` ext, `enabled` and `std_rng` dropped), stations migrated, CI `check-no-sim` guard.
2. Core `signal_gauge` surface + `Observable` kernel reshape + `.observe()`/`.log()` ext, hub migrated, README `Observable` row.
3. `LinkableRegistrarExt` + `#[derive(Linkable)]`, mqtt demo migrated.
4. `aimdb-sync` `set_value` family + `settable` feature gate + tests.
5. README verb/tier tables + CHANGELOG sweep.

**Out of scope / recorded for later:** runtime sim↔real failover (a single source future multiplexing on sensor staleness — still single-writer-safe, own doc); breaking replacement of the legacy `String` errors across `Linkable` and connector deserializer signatures; AimX set-by-primitive verb (second `Settable` consumer); signal-metrics history/percentiles beyond last/min/max/mean; auto-migration on the `linked_from` path (§3.6).
