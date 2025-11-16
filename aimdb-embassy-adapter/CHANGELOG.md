# Changelog - aimdb-embassy-adapter

All notable changes to the `aimdb-embassy-adapter` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
