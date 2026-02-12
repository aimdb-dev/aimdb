# Changelog - aimdb-tokio-adapter

All notable changes to the `aimdb-tokio-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
