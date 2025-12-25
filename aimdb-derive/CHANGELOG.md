# Changelog - aimdb-derive

All notable changes to the `aimdb-derive` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

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
