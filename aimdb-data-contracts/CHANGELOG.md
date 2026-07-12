# Changelog

All notable changes to `aimdb-data-contracts` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking) — Design 041: capability traits as first-class verbs

- **`Simulatable` reshaped for compile-time-only simulation.** `simulate<R>(config: &SimulationConfig, ...)` → `simulate<R>(params: &Self::Params, ...)` with a new associated `type Params`. `SimulationConfig` (including its runtime `enabled` gate) and `SimulationParams` are removed; `SimProfile<P> { interval_ms, params }` and off-the-shelf `RandomWalkParams` replace them (`type Params = RandomWalkParams` is the migration path for existing scalar-walk impls). `simulatable` now also requires `aimdb-core` (for the new `SimulatableRegistrarExt`) and `rand` drops the `std_rng` feature (RNG is always caller-supplied).
- **`Observable` loses `ICON` and `format_log()`.** The trait is now numeric-projection-only: `type Signal`, `const SIGNAL` (defaults to `Self::NAME`), `const UNIT`, `fn signal()`. Presentation moved to `ObservableRegistrarExt::log(node_id)`.
- **`Settable` moves behind a new `settable` feature** (was compiled unconditionally) for tier symmetry with the other wire contracts.
- **`linkable` is now format-neutral.** It enables only the `Linkable` trait, registrar verbs, `alloc`, and `aimdb-core`; it no longer pulls `serde_json` or re-exports the JSON `#[derive(Linkable)]`. Existing derive users must enable the new `linkable-json` feature. Manual Postcard/custom implementations stay on base `linkable`, so their normal dependency graph is JSON-free (#155).

### Added

- **Issue #177 — `Linkable::encode_into(&mut [u8])`.** Existing implementations
  remain source-compatible through a checked `to_bytes`-and-copy default. A new
  `ENCODE_BUFFER_CAPACITY` associated constant lets implementations advertise a
  real allocation-free override; `linked_to` then installs the core's reusable
  scratch-buffer fast path while retaining `to_bytes` for oversized values.
  The new method uses `aimdb_core::connector::LinkCodecError`; the older
  `from_bytes`/`to_bytes` `String` errors remain unchanged pending a separately
  agreed breaking migration.
- `SimulatableRegistrarExt::simulate(profile, rng)` — installs a `.source()` loop emitting `T::simulate(...)` on a timer.
- `ObservableRegistrarExt::observe()` — feeds `T::signal()` into a core signal gauge (last/min/max/mean), surfaced on `record.list`/`record.get`/stage profiling; `ObservableRegistrarExt::log(node_id)` for the console-logging path (replaces the old `format_log`).
- `LinkableRegistrarExt::linked_from(url)` / `linked_to(url)` — one-line `.link_from()`/`.link_to()` wiring defaulted to `T::from_bytes`/`T::to_bytes`. `linkable` now also requires `aimdb-core`.
- `linkable-json` — layers `serde_json` and the JSON `#[derive(Linkable)]` convenience over the format-neutral `linkable` contract; `no_std + alloc` compatible.

### Changed

- **`migratable` no longer requires `std`.** `migratable = ["alloc", "serde_json"]` (was `["std", "serde_json"]`); the crate's `serde_json` dependency now follows the workspace pin (`default-features = false, features = ["alloc"]`) instead of pulling in `serde_json/std` by default. `migration_chain!` and both `Linkable`-with-migration patterns now compile on `no_std + alloc` (e.g. `thumbv7em-none-eabihf`). A crate that enabled only `migratable` and relied on it transitively activating `std` will need to enable `std` explicitly. In-repo consumers are unaffected (they enable `std` by default).
- `log_tap<T, R>(ctx, consumer, node_id)` now takes `Consumer<T>` instead of `Consumer<T, R>` — `R` is still on `RuntimeContext<R>` (Design 029, M14). User-visible signature shrink only; behaviour unchanged.

## [0.1.1] - 2026-05-22

### Changed

- **Dependency update**: Upgraded `rand` from 0.8 to 0.10.1.

## [0.1.0] - 2026-03-16

### Added

- `Streamable` trait — capability marker for types crossing serialization boundaries (WebSocket, WASM, wire)
- `Migratable` trait with `MigrationChain` and `MigrationStep` for schema evolution
- Explicit version pins for path dependencies (`aimdb-core = "1.0.0"`, `aimdb-executor = "0.1.0"`)

### Changed

- **Breaking**: Refocused as a pure trait-definition crate
  - Removed concrete contracts (`Temperature`, `Humidity`, `GpsLocation`) — moved to application-level crates (e.g., `weather-mesh-common`)
  - Removed closed `StreamableVisitor` dispatcher and `for_each_streamable()` — replaced by extensible registry pattern in connector/adapter crates
  - Removed `ts` feature and `ts-rs` dependency
  - Version reset from 1.0.0 to 0.1.0 to reflect the reduced, stabilizing scope

## [0.5.0] - 2026-02-21

### Added

- Initial release with shared data contract types
- `SchemaType` trait for compile-time type identity
- `Linkable` trait for wire format support in connector transport
- `Simulatable` trait with `SimulationConfig` and `SimulationParams` for test data generation
- `Observable` module with `log_tap` function for runtime observability
- Built-in contracts: `Temperature`, `Humidity`, `GpsLocation`
- Feature flags: `linkable`, `simulatable`, `migratable`, `observable`, `ts`
