# Changelog - aimdb-embassy-adapter

All notable changes to the `aimdb-embassy-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
