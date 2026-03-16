# Changelog

All notable changes to `aimdb-data-contracts` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [0.1.0] - 2026-03-16

### Added

- `Streamable` trait — capability marker for types crossing serialization boundaries (WebSocket, WASM, wire)
- `Migratable` trait with `MigrationChain` and `MigrationStep` for schema evolution
- Explicit version pins for path dependencies (`aimdb-core = "1.0.0"`, `aimdb-executor = "0.1.0"`)

### Changed

- **Breaking**: Refocused as a pure trait-definition crate
  - Removed concrete contracts (`Temperature`, `Humidity`, `GpsLocation`) — moved to application-level crates (e.g., `weather-mesh-common`)
  - Removed closed `StreamableVisitor` dispatcher and `for_each_streamable()` — replaced by extensible registry pattern in connector/adapter crates
  - Removed `ts` feature and `ts-rs` dependency
  - Version reset from 1.0.0 to 0.1.0 to reflect the reduced, stabilizing scope

## [0.5.0] - 2026-02-21

### Added

- Initial release with shared data contract types
- `SchemaType` trait for compile-time type identity
- `Linkable` trait for wire format support in connector transport
- `Simulatable` trait with `SimulationConfig` and `SimulationParams` for test data generation
- `Observable` module with `log_tap` function for runtime observability
- Built-in contracts: `Temperature`, `Humidity`, `GpsLocation`
- Feature flags: `linkable`, `simulatable`, `migratable`, `observable`, `ts`
