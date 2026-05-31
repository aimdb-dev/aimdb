# Changelog - aimdb-embassy-adapter

All notable changes to the `aimdb-embassy-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Session-engine smoke test on the Embassy clock (Issue #39, Phase 5, [design doc](../docs/design/remote-access-via-connectors.md)).** New `tests/session_smoke.rs` drives `aimdb-core`'s runtime-neutral `run_client` engine using the `EmbassyAdapter`'s `TimeOps` clock for reconnect backoff / keepalive — proving the shared session engines run on Embassy, not just Tokio. Dev-only: pulls in `aimdb-core` with the `connector-session` feature, so the normal `no_std` lib build and the `thumbv7em` cross-checks stay `alloc`-only.
- **`EmbassyBuffer::peek()` (M15, Design 031).** Non-destructive buffer-native read matching the Tokio adapter's semantics: `SingleLatest` (`Watch`) via `Watch::try_get()`, `Mailbox` (`Channel<_, T, 1>`) via `Channel::try_peek()`, `SpmcRing` (`PubSubChannel`) returns `None`. Neither path consumes a receiver slot or advances a cursor.
- **Embassy buffer + join-queue unit tests now run in CI on the host (Issue #85).** Previously the join-queue tests sat behind `feature = "embassy-runtime"`, which transitively pulls `embassy-executor`'s `platform-cortex-m` ARM assembly and fails to compile under `cargo test` on x86_64 — so ordering / backpressure / clone-routing regressions went uncaught. The `join_queue` module is now gated on `embassy-sync` instead (the `JoinFanInRuntime for EmbassyAdapter` impl keeps its own `embassy-runtime` gate), and `make test` runs `cargo test -p aimdb-embassy-adapter --no-default-features --features "alloc,embassy-sync,embassy-time"` (15 unit tests + doctests). A test-only no-op `#[defmt::global_logger]` / `#[defmt::panic_handler]` and a trivial `embassy-time-driver` satisfy the host link targets that `defmt` + `defmt-timestamp-uptime` would otherwise leave undefined.
- **`embassy-time-driver` dev-dependency** — provides the trivial host time driver above (no tick feature, so it unifies with the workspace `tick-hz-32_768` rather than forcing `mock-driver`/`std`'s conflicting rate).

### Fixed

- **`TypedRecord::latest()` no longer always returns `None` on Embassy (M15).** With `latest_snapshot` removed in `aimdb-core`, reads go straight to the buffer via `peek()`; the Embassy adapter now implements `peek()` (above) so `latest()` returns the current value on `SingleLatest` / `Mailbox` instead of `None`.
- **Stale `EmbassyBuffer` doc example.** It imported the removed `BufferBackend` trait (now `Buffer` / `BufferReader`) and put a non-`const` `new_spmc()` in a `static`; it never compiled because doctests didn't build on host before. Now corrected and exercised by `cargo test`'s doctest pass.

### Changed

- **`buffer()` / `buffer_sized()` now record the `BufferCfg` (via `buffer_with_cfg`).** `buffer_info()` therefore reports the real buffer type and capacity in the dependency graph on `no_std` too (previously `"unknown"`), matching std behaviour. Mirrors the `aimdb-core` `impl_record_registrar_ext!` change (M15).

### Changed (breaking)

- **Generated extension trait emits `Producer<T>` / `Consumer<T>`** (no `, EmbassyAdapter`) via the updated `impl_record_registrar_ext!` macro from `aimdb-core` (Design 029, M14). Embassy demo signatures collapse from `Producer<LightControl, EmbassyAdapter>` to `Producer<LightControl>`.

### Removed (breaking)

- **`impl Spawn for EmbassyAdapter` deleted (Issue #88).** Static `generic_task_runner` task pool gone, along with `BoxedFuture` and the `unsafe Pin::new_unchecked` cast that fed the pool. Drive database futures by awaiting `AimDbRunner::run()` from inside the Embassy main task.
- **`embassy-task-pool-8` / `embassy-task-pool-16` / `embassy-task-pool-32` Cargo features deleted.** No pool — `FuturesUnordered` grows as needed within a single Embassy task's heap budget.
- **`EmbassyAdapter::new_with_spawner(spawner)` constructor deleted.**
- **`EmbassyAdapter::new_with_network(spawner, network)` signature changed** to `new_with_network(network)` — the `spawner` argument is gone. Update callers (three example binaries plus aimdb-pro docs).
- `spawner: Option<Spawner>` field and `spawner()` accessor deleted.

### Notes

- `unsafe impl Send/Sync for EmbassyAdapter` is **retained** when the `embassy-net-support` feature is enabled: `embassy_net::Stack` contains a `RefCell` and is `!Sync`. Embassy's single-threaded cooperative executor makes this sound, and the impl now has a smaller surface (no `Spawner` to justify it).

## [0.6.0] - 2026-05-22

### Added

- **`profiling` feature** (Issue #58): Forwards to `aimdb-core/profiling` and enables the Embassy runtime clock for stage timing. Pulls `portable-atomic` with `fallback` + `critical-section` (via `aimdb-core/profiling`) to emulate 64-bit atomics on targets without native `AtomicU64` (e.g. `thumbv7em-none-eabihf`). The final binary must provide a `critical-section` implementation — cortex-m and Embassy HALs already do.
- **`TimeOps::duration_as_nanos` implementation**: Computes `duration.as_micros() * 1_000` (saturating). Microsecond resolution is the portable lower bound across Embassy tick-rate configurations.
- **Cross-compile check in `make test-embedded`**: `cargo check --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,profiling"` to guard the no_std + profiling path.
- **`EmbassyJoinQueue` (Design 027)**: Embassy implementation of the `JoinFanInRuntime` traits from `aimdb-executor`, backed by `embassy_sync::channel::Channel<CriticalSectionRawMutex, T, 8>`. The channel is `Box::leak`ed at queue creation (once per join transform at DB startup) to obtain the `&'static` lifetime Embassy channels require. Embassy channels never close — the trigger loop runs for the device lifetime.
- **`SpmcRing` subscriber-slot exhaustion diagnostics**: `defmt::error!` now fires when a `.subscribe()` call fails because the const-generic `SUBS` slot count is exhausted. Includes guidance to count one slot per `.link_to()` plus one per `transform_join` input.
- **Improved `buffer_sized<CAP, CONSUMERS>` doc**: explicit rules for counting `CONSUMERS` (one per `.tap()`, `.link_to()`, and `transform_join` input).

### Changed

- **`aimdb-executor` dependency**: dropped the `embassy-types` feature (no longer required — the join queue is implemented locally in this adapter using `embassy_sync::Channel` directly).
- **Dev-dependency update**: Upgraded `rand` from 0.8 to 0.10.1.
- **Dev-dependency added**: `critical-section` with `std` feature, providing the `CriticalSectionRawMutex` link target for host-side join-queue tests.

## [0.5.0] - 2026-02-21

### Changed

- **Dependency Update**: Updated `aimdb-core` dependency to 0.5.0

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-core` dependency to 0.4.0 for RecordKey trait support

## [0.3.0] - 2025-12-15

### Added

- **DynBuffer Explicit Implementation**: `EmbassyBuffer` now explicitly implements `DynBuffer<T>` (required due to removal of blanket impl in aimdb-core)
- **Metrics Feature Placeholder**: Added `metrics` feature flag (non-functional placeholder for API consistency). Actual metrics support requires std and is not available on embedded targets.

### Changed

- **Breaking: DynBuffer Implementation**: `EmbassyBuffer` now has an explicit `DynBuffer` implementation instead of relying on the removed blanket impl. `metrics_snapshot()` returns `None` on Embassy (metrics not supported on embedded).

## [0.2.0] - 2025-11-20

### Added

- **Network Stack Access**: New `EmbassyNetwork` trait enables connectors to access Embassy's network stack for network-dependent operations
- Network stack integration for connectors requiring TCP/UDP communication

### Changed

- Enhanced runtime adapter with improved task management for connector support
- Updated Embassy submodule to latest commit with improved async runtime support
- Updated connector integration to support new `ConnectorBuilder` pattern

## [0.1.0] - 2025-11-06

### Added

- Initial release of Embassy runtime adapter for embedded AimDB deployments
- Configurable task pool sizes (8/16/32 concurrent tasks via feature flags)
- Optimized for resource-constrained devices
- Compatible with ARM Cortex-M targets (`thumbv7em-none-eabihf`, `thumbv8m.main-none-eabihf`)
- `no_std` compatibility with `alloc` support
- Simplified buffer API with 2-parameter configuration
- Task spawning for Embassy executor
- Time operations with Embassy's async Timer
- Logging integration with defmt

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
