# Changelog - aimdb-client

All notable changes to the `aimdb-client` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking: RecordMetadata Field Rename (via aimdb-core)**: Re-exported `RecordMetadata` type now has `connector_count` field renamed to `outbound_connector_count`. This change originates from `aimdb-core` and affects code accessing this field through `aimdb-client`.

## [0.1.0] - 2025-11-06

### Added

- Initial release of AimDB client library
- Reusable connection and discovery logic for remote AimDB instances
- Unix domain socket communication
- AimX v1 protocol implementation
- Clean error handling with typed errors
- Instance discovery via socket scanning
- Record querying and value retrieval
- Support for subscription management

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
