# Changelog - aimdb-tokio-adapter

All notable changes to the `aimdb-tokio-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed (breaking)

- **`TokioDatabase` type alias removed (Issue #132, design 034 Phase 1).** It aliased the dead `aimdb_core::Database<TokioAdapter>` wrapper (also removed) and had no users in the workspace. Use `AimDb<TokioAdapter>` via `AimDbBuilder`.

### Added

- **`TimeOps::unix_time()` implemented from the OS wall clock (Issue #120).** Returns `SystemTime::now()` since the Unix epoch as `(secs, subsec_nanos)`; `now()` stays monotonic for duration measurement. Supplies absolute timestamps to the runtime-neutral AimX server / remote-display paths.
- **`TokioBuffer::peek()` (M15, Design 031).** Non-destructive buffer-native read backing AimX `record.get` / `TypedRecord::latest()`: `SingleLatest` (`Watch`) reads via `watch::Sender::borrow()`, `Mailbox` (`Notify`) clones the slot mutex, `SpmcRing` (`Broadcast`) returns `None` (no canonical latest). Unit tests cover all three buffer types (empty, populated, non-destructive, overwrite, drained).
- **`tests/remote_access_validation.rs` integration test.** Asserts that a `.with_remote_access()` record with no buffer fails `build()`, and that the same record with a `SingleLatest` buffer builds — locking in the new build-time guard from `aimdb-core` (M15).

### Fixed

- **`SingleLatest` no longer drops a value produced before any subscriber attached (M15).** The `Watch` push path now uses `watch::Sender::send_replace` instead of `send`. `send` returns `Err` and discards the value when there are zero receivers; `send_replace` always updates the slot, so the value is visible to `peek()` and to later subscribers reading the slot.

### Changed (breaking)

- **Generated extension trait emits `Producer<T>` / `Consumer<T>`** (no `, TokioAdapter`) via the updated `impl_record_registrar_ext!` macro from `aimdb-core` (Design 029, M14). User-side `.source(|ctx, producer| ...)` / `.tap(|ctx, consumer| ...)` callbacks now receive the simpler types.

### Removed (breaking)

- **`impl Spawn for TokioAdapter` deleted (Issue #88).** Adapter is now `RuntimeAdapter + TimeOps + Logger` only. Drive database futures by awaiting `AimDbRunner::run()` returned from `AimDbBuilder::build()`.
- **`TokioAdapter::spawn_connectors` test-only helper deleted** along with the `connector` module. It had no production callers; outbound connector futures are now collected by `ConnectorBuilder::build()` and driven by the runner.

### Notes

- **Drain integration tests now stand up the AimX server via `aimdb-uds-connector::UdsServer`** (new dev-dependency) instead of the removed `AimDbBuilder::with_remote_access(config)`, and connect with the engine-based `aimdb-client::AimxConnection` (Issue #39). Test-only; no production change.
- `BufferOps::spawn_dispatcher` (a test-only utility) is unchanged — it calls `tokio::spawn` directly and does not depend on the deleted `Spawn` trait.
- `tests/stage_profiling.rs` dropped the `avg == total / call_count` assertion: it is a tautology (`avg_time_ns()` is *defined* as that quotient) and racy while the source task is still producing. No coverage lost.

## [0.6.0] - 2026-05-22

### Added

- **`profiling` feature** (Issue #58): Forwards to `aimdb-core/profiling`. Enables automatic per-stage wall-clock timing for `.source()`, `.tap()`, and `.link()` on the Tokio runtime. Off by default; zero overhead when disabled.
- **`TimeOps::duration_as_nanos` implementation**: Returns `Duration::as_nanos()` saturated to `u64::MAX`. Required by the new `aimdb-executor` trait method.
- **Stage profiling integration test** (`tests/stage_profiling.rs`, gated on `--features profiling`): Drives a record with a periodic `.source()` and a slow `.tap()`, then asserts that call counts are recorded, the `min ≤ avg ≤ max` invariant holds, `.with_name(...)` is reflected on the registered stages, and `reset_all()` clears the counters.
- **`TokioJoinQueue` (Design 027)**: Tokio implementation of the `JoinFanInRuntime` traits from `aimdb-executor`, backed by `tokio::sync::mpsc::channel` with internal capacity 64. Enables `transform_join` on the Tokio runtime through the new runtime-agnostic abstraction.
- **`transform_join` integration tests** (`tests/transform_join_integration_tests.rs`): two-input sum scenario plus a backpressure stress test that pushes 200 events through a yielding handler to verify the bounded fan-in does not deadlock.

## [0.5.0] - 2026-02-21

### Added

- **`try_recv()` for TokioBufferReader**: Non-blocking receive for all buffer types
  - Returns `Ok(T)` if value available, `Err(BufferEmpty)` if none, `Err(BufferLagged)` on ring overflow
  - Supports `SpmcRing` (drains all pending), `SingleLatest` (changed value only), `Mailbox` (takes slot)
  - Enables drain-loop pattern for batch processing accumulated values
  - Full metrics tracking for consumed and dropped counts
- **Comprehensive try_recv Tests**: 15+ tests covering all buffer types, edge cases, and metrics
- **Drain Integration Tests**: `drain_integration_tests.rs` with full AimX protocol drain testing
  - Tests cold start, accumulation, sequential drains, limits, overflow recovery
  - Tests multi-record independence, SingleLatest behavior, error cases
  - Tests `with_remote_access()` requirement and response structure

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-core` dependency to 0.4.0 for RecordKey trait support

## [0.3.0] - 2025-12-15

### Added

- **Buffer Metrics Implementation**: Full `BufferMetrics` trait implementation for `TokioBuffer` when `metrics` feature is enabled
  - Tracks `produced_count`, `consumed_count`, `dropped_count` via atomic counters
  - Reports real-time `occupancy` for all buffer types (SPMC Ring, SingleLatest, Mailbox)
  - `reset_metrics()` method for windowed metrics collection
- **Comprehensive Metrics Tests**: New `metrics_tests` module with tests for all buffer types and edge cases
- **DynBuffer Explicit Implementation**: `TokioBuffer` now explicitly implements `DynBuffer<T>` with `metrics_snapshot()` support
- **Multi-Instance Record Tests**: Comprehensive test suite (`multi_instance_tests.rs`) validating RecordId/RecordKey architecture:
  - Tests for multiple records of same type with different keys
  - Tests for key-based producer/consumer APIs
  - Tests for error cases (AmbiguousType, RecordKeyNotFound, TypeMismatch)
  - Tests for RecordId stability and introspection

### Changed

- **Breaking: DynBuffer Implementation**: `TokioBuffer` now has an explicit `DynBuffer` implementation instead of relying on the removed blanket impl. This is transparent to users but enables metrics support.
- **Connector Introspection**: Updated to use `by_key` and record iteration by RecordId instead of TypeId-based iteration

## [0.2.0] - 2025-11-20

### Changed

- **Breaking: Connector API Update**: Updated internal connector introspection to use renamed methods:
  - `record.connector_count()` → `record.outbound_connector_count()`
  - `record.connector_urls()` → `record.outbound_connector_urls()`
- Updated connector integration to support new `ConnectorBuilder` pattern
- Enhanced runtime adapter to work with async connector initialization

## [0.1.0] - 2025-11-06

### Added

- Initial release of Tokio runtime adapter for AimDB
- Lock-free buffer implementations
- Configurable buffer capacities
- Comprehensive async task spawning
- Full std library support
- Time operations with Tokio's async sleep
- Logging integration with tracing

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
