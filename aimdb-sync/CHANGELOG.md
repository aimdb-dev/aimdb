# Changelog - aimdb-sync

All notable changes to the `aimdb-sync` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`SyncProducer<T: Settable>::set_value`/`try_set_value`/`set_value_at`** (design 041 §3.4, feature `data-contracts`). `set()` took a fully constructed `T`, so every outside-the-thread caller hand-assembled the struct; `set_value(value)` constructs via `T::set(value, timestamp)` and sends in one call — blocking (`set_value`), non-blocking (`try_set_value`), or with an explicit timestamp for replay/testing (`set_value_at`). `set_value`/`try_set_value` stamp the caller's `SystemTime` (crate is std-only). New optional dependency: `aimdb-data-contracts` (feature `settable`), behind the new `data-contracts` feature — the contracts crate gains no `sync` feature (dependency direction unchanged).

### Changed (breaking)

- **Issue #131:** `AimDbSyncExt` extends the non-generic `aimdb_core::AimDb`; internal handles drop the `TokioAdapter` type parameter.

### Changed

- `attach()` updated to destructure the `(AimDb<TokioAdapter>, AimDbRunner)` tuple returned by `AimDbBuilder::build()` after issue #88, and to drive the runner inside the runtime thread via `tokio::select!` against the shutdown signal. No public API change.

## [0.5.0] - 2026-02-21

### Changed

- **Dependency Update**: Updated `aimdb-core` and `aimdb-tokio-adapter` dependencies to 0.5.0

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-core` and `aimdb-tokio-adapter` dependencies to 0.4.0

## [0.3.0] - 2025-12-15

### Changed

- **Breaking: Producer/Consumer API**: All methods now require a record key parameter:
  - `producer::<T>(key)` instead of `producer::<T>()`
  - `consumer::<T>(key)` instead of `consumer::<T>()`
  - `producer_with_capacity::<T>(key, capacity)` instead of `producer_with_capacity::<T>(capacity)`
  - `consumer_with_capacity::<T>(key, capacity)` instead of `consumer_with_capacity::<T>(capacity)`
- **Breaking: Record Registration API**: Updated all test code to use new key-based `configure<T>(key, |reg| ...)` API
- All integration tests now specify explicit record keys (e.g., `"test.data"`) per new RecordId/RecordKey architecture

## [0.2.0] - 2025-11-20

### Changed

- Updated to support async `build()` method in `aimdb-core`
- Compatible with new connector builder pattern

## [0.1.0] - 2025-11-06

### Added

- Initial release of synchronous API wrapper for AimDB
- Blocking wrapper around async AimDB core
- Thread-safe synchronous record access
- Automatic Tokio runtime management
- Ideal for gradual migration from sync to async
- Type-safe synchronous record operations
- Compatible with existing synchronous codebases

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
