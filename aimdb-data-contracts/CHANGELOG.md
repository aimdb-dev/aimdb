# Changelog

All notable changes to `aimdb-data-contracts` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [1.0.0] - 2026-03-11

### Added

- `Streamable` trait and `StreamableVisitor` pattern for type-erased dispatch across WebSocket, WASM, and other wire boundaries
- `for_each_streamable()` dispatcher function used by WASM adapter, WebSocket connector, and CLI
- `Migratable` trait with `MigrationChain` and `MigrationStep` for schema evolution

### Changed

- `Temperature` contract expanded with simulation support

## [0.5.0] - 2026-02-21

### Added

- Initial release with shared data contract types
- `SchemaType` trait for compile-time type identity
- `Linkable` trait for wire format support in connector transport
- `Simulatable` trait with `SimulationConfig` and `SimulationParams` for test data generation
- `Observable` module with `log_tap` function for runtime observability
- Built-in contracts: `Temperature`, `Humidity`, `GpsLocation`
- Feature flags: `linkable`, `simulatable`, `migratable`, `observable`, `ts`
