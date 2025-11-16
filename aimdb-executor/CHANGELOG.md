# Changelog - aimdb-executor

All notable changes to the `aimdb-executor` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes in this release.

## [0.1.0] - 2025-11-06

### Added

- Initial release of runtime executor abstraction traits
- `RuntimeAdapter` trait for platform identification
- `Spawn` trait for async task spawning
- `TimeOps` trait for time operations (now, sleep)
- `Logger` trait for logging abstraction
- `no_std` compatibility for embedded targets
- Cross-platform support for Tokio and Embassy runtimes

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
