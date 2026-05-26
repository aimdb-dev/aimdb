# Changelog

All notable changes to `aimdb-wasm-adapter` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed (breaking)

- **`impl Spawn for WasmAdapter` deleted (Issue #88).** Also removed the `unsafe impl Send/Sync for WasmAdapter` — the adapter is a ZST and auto-derives both.
- `bindings.rs::Database::build()` now consumes the `AimDbRunner` returned from `AimDbBuilder::build()` and drives it via `wasm_bindgen_futures::spawn_local`, so existing JS callers keep working with no API change.

## [0.2.0] - 2026-05-22

### Added

- **`TimeOps::duration_as_nanos` implementation**: Converts `WasmDuration` (milliseconds as `f64`) to nanoseconds, clamped to `[0, u64::MAX]`. Required by the new `aimdb-executor` trait method (used by stage profiling, Issue #58).
- **`WasmJoinQueue` (Design 027)**: WASM implementation of the `JoinFanInRuntime` traits from `aimdb-executor`, backed by `futures_channel::mpsc::channel` with internal capacity 64. Enables `transform_join` on the WASM runtime.
- **`transform_join` integration test** (`tests/transform_join_integration_tests.rs`, `wasm-bindgen-test`): two-input sum scenario verifying outputs are produced once both inputs have been seen.

### Changed

- **Dependencies**: added `futures-channel` (with `std` + `sink` features — `mpsc` requires `std` because its internal `BiLock` uses `std::sync`); enabled `sink` on `futures-util`.

## [0.1.1] - 2026-03-16

### Added

- Initial release of the AimDB WebAssembly runtime adapter
- Full `aimdb-executor` trait implementations: `RuntimeAdapter`, `Spawn`, `TimeOps`, `Logger`
- `Rc<RefCell<…>>` buffer implementation — zero-overhead for single-threaded WASM
- All three buffer types: SPMC Ring, SingleLatest, Mailbox
- `WasmDb` facade via `#[wasm_bindgen]`: `configureRecord`, `get`, `set`, `subscribe`
- `WsBridge` — WebSocket bridge connecting browser to remote AimDB server
- `SchemaRegistry` for type-erased record dispatch with extensible `.register::<T>()` API
- React hooks: `useRecord<T>`, `useSetRecord<T>`, `useBridge`
- `no_std` + `alloc` compatible (`wasm32-unknown-unknown` target)
