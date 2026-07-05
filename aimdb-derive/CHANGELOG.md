# Changelog - aimdb-derive

All notable changes to the `aimdb-derive` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`migration_chain!` proc-macro (Design 039, PR2).** Variable-arity replacement for the 3-arm `macro_rules!` previously hand-unrolled in `aimdb-data-contracts`; re-exported as `aimdb_data_contracts::migration_chain!` with the same grammar and call path. Generated dispatch is `O(N)` in code size regardless of chain length (one `__up_k`/`__down_k` helper per step). Emits foreign-crate paths into `aimdb_data_contracts` without depending on it (same pattern as `RecordKey` → `aimdb_core`) — a build-time-only dependency with no target/runtime/`no_std` impact.
- **Tree-free version probe in `migrate_from_bytes` (Design 039, PR3).** The generated dispatch no longer parses the payload into a full `serde_json::Value` tree to read the version field. It now scans a small `#[derive(serde::Deserialize)]` probe struct for just the version, then parses the same bytes a second time directly into the matched concrete type — peak allocation drops from O(payload tree) to O(concrete struct). `serde_json::Error::is_data()` preserves the existing `MissingVersion` vs `DeserializationFailed` split (missing/wrong-type field vs malformed JSON). Wire behavior is unchanged (same errors for missing/unknown versions).

### Fixed

- **`migrate_from_bytes` reports `VersionTooOld` for below-minimum source versions.** A payload whose version is below `MIN_VERSION` (e.g. `0`) previously fell through to `VersionTooNew` (`"source version 0 is newer than current N"`, contradictory); the generated dispatch now returns `VersionTooOld { target, minimum }`, matching the `migrate_to_version` path. Covered by a new `error_on_source_version_too_old` round-trip test.

## [0.1.0] - 2025-12-23

### Added

- **Initial Release**: `#[derive(RecordKey)]` macro for compile-time checked record keys
- **Attributes**:
  - `#[key = "..."]` (required): String representation for each variant
  - `#[key_prefix = "..."]` (optional): Namespace prefix applied to all variants
  - `#[link_address = "..."]` (optional): Connector metadata (MQTT topics, KNX addresses)
- **Generated Implementations**:
  - `impl RecordKey` with `as_str()` and `link_address()` methods
  - `impl Borrow<str>` for O(1) HashMap lookups
  - `impl Hash` that hashes the string key (satisfies `Borrow<str>` contract)
- **Compile-Time Validation**:
  - Duplicate key detection
  - Unit variant enforcement (no tuple/struct variants)
  - Missing `#[key]` attribute detection
- **no_std Support**: Fully compatible with `no_std` environments

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
