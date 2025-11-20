# Changelog - aimdb-sync

All notable changes to the `aimdb-sync` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
