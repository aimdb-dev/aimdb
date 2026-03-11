# Changelog

All notable changes to `aimdb-wasm-adapter` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [0.1.1] - 2026-03-11

### Added

- Initial release of the AimDB WebAssembly runtime adapter
- Full `aimdb-executor` trait implementations: `RuntimeAdapter`, `Spawn`, `TimeOps`, `Logger`
- `Rc<RefCell<…>>` buffer implementation — zero-overhead for single-threaded WASM
- All three buffer types: SPMC Ring, SingleLatest, Mailbox
- `WasmDb` facade via `#[wasm_bindgen]`: `configureRecord`, `get`, `set`, `subscribe`
- `WsBridge` — WebSocket bridge connecting browser to remote AimDB server
- `SchemaRegistry` for type-erased record dispatch
- React hooks: `useRecord<T>`, `useSetRecord<T>`, `useBridge`
- `no_std` + `alloc` compatible (`wasm32-unknown-unknown` target)
