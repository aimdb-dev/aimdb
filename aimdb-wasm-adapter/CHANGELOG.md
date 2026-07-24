# Changelog

All notable changes to `aimdb-wasm-adapter` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking) — Design 045: `WsBridge` is an AimX engine client

- **`WsBridge` rewritten on `run_client` + `ClientHandle`** over a
  `web_sys::WebSocket`-backed `Connection`/`Dialer`: reply/subscription
  correlation, reconnect backoff, keepalive, and the offline queue now live in
  `aimdb-core`'s client engine — the hand-rolled 835-line demux is gone. The
  `#[wasm_bindgen]` surface is preserved (`write`, `query`, `listTopics`,
  `onStatusChange`, `status`, `disconnect`, `connectBridge` options), but the
  wire is AimX, so the bridge only talks to design-045 servers.
  `BridgeOptions.lateJoin` is retained for option-shape compatibility
  (snapshots are server-driven under AimX).
- **`WasmDb.discover` / the raw discovery path speak `record.list`** and
  resolve with `{name, schema_type, entity}` rows.
- Depends on `aimdb-core`'s `connector-session` + `remote` features (the
  engines cross-compile to wasm32); the `aimdb-ws-protocol` dependency is
  gone.

### Fixed

- **SingleLatest fresh-subscriber parity (Design 040).** A subscriber created
  *after* a value has been published now receives that current value on its
  first `recv()` / `try_recv()`, instead of waiting for the next push. This
  matches the tokio and embassy adapters and is the documented cross-runtime
  contract — a dashboard that subscribes to a config record after the producer
  published now renders the current state on mount rather than blank until the
  next change. `WasmBuffer::subscribe()` initializes the reader as
  never-having-seen-anything (`last_seen_version: 0`) rather than snapshotting
  the current version.
- **`peek()` on WASM buffers (Design 040).** `WasmBuffer` now overrides
  `DynBuffer::peek()`, returning the current value for `SingleLatest` and the
  pending slot for `Mailbox` (`None` for `SpmcRing`), matching tokio/embassy.
  The buffer-native non-destructive read used by AimX `record.get` previously
  returned `None` on WASM.

### Changed (behavioral)

- **JS/WASM `SingleLatest` subscribers may observe one extra initial delivery.**
  As a consequence of the fresh-subscriber fix above, `subscribe` /
  `subscribe_typed` (bindings and `WsBridge`) now fire an immediate callback with
  the current value when one already exists. Portable code already tolerates
  this (the other two runtimes behave this way); only code that treated every
  delivery as a *transition* is affected.

### Added

- **Host-run buffer unit tests + shared contract suite (Design 040).** `buffer.rs`
  now has native (`cargo test`) unit tests for the fresh-subscriber and `peek()`
  semantics, plus the `aimdb-core` `buffer::test_support` conformance suite run
  under `futures::executor::block_on` — the same suite the tokio and embassy
  adapters run. Added `futures` as a host-only (wasm32-excluded) dev-dependency.

### Changed (breaking)

- **Issue #131 — `WasmRecordRegistrarExt` shrinks to `.buffer(cfg)` only** (`source`/`tap`/`transform` are inherent on the non-generic `RecordRegistrar<'a, T>`); `join_queue.rs` (`WasmJoinQueue`) is deleted with the `JoinFanInRuntime` family; bindings/bridge take the non-generic `AimDb`.

### Added

- **`RuntimeOps` implemented for `WasmAdapter` (Issue #130, design 034 Phase 2).** The dyn-safe capability surface from `aimdb-executor`: `now_nanos()` from `Performance.now()` (monotonic ms since page load), `unix_time()` from `Date.now()` (wall clock — the generic `TimeOps::unix_time` default still returns `None`), `sleep` boxes the setTimeout-Promise future, `log` forwards to the console-backed `Logger`. Browser contract test via `wasm_bindgen_test`; native fallback covered by a sync-surface test.

### Changed

- **`record.set` write path routes through `Producer<T>` (M15, Design 031).** `bindings.rs` (`set`) and `ws_bridge.rs` now call `db.producer::<T>(key)?.produce(val)` instead of the removed `TypedRecord::produce`. Internal only — no `#[wasm_bindgen]` / JS API change.

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
