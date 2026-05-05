# Changelog

All notable changes to `aimdb-wasm-adapter` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
