# Changelog - aimdb-tokio-adapter

All notable changes to the `aimdb-tokio-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
