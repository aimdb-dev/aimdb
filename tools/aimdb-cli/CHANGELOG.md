# Changelog - aimdb-cli

All notable changes to the `aimdb-cli` tool will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Global `--connect <endpoint>` flag + `AIMDB_CONNECT` env (Issue #123).** Choose the target instance by `scheme://` URL — `unix://PATH`, `serial://DEVICE?baud=N` (with the `transport-serial` feature), or a bare path (the `unix://` shorthand). Precedence: `--connect` → `AIMDB_CONNECT` → UDS auto-discovery. `instance info`/`ping` now work over any endpoint (not just discovered sockets). New `transport-serial` feature (off by default; pulls libudev) adds the serial transport to the resolver.

### Changed (breaking)

- **Per-command `--socket` flags removed in favor of the global `--connect` (Issue #123).** `aimdb record/graph/watch/instance … --socket <path>` is gone; pass `--connect <endpoint>` (a bare path still works) or rely on `AIMDB_CONNECT`/auto-discovery. `instance list` remains discovery-only.

### Changed

- **Migrated to the engine-based `aimdb-client::AimxConnection` (Issue #39).** All commands (`watch`, `record`, `graph`) now use `AimxConnection` instead of the retired `AimxClient`, speaking the reshaped **AimX-v2** protocol. `aimdb watch` subscribes via the engine, which streams updates routed by request id — there is no server-allocated subscription id to display, and `--queue-size` is accepted for compatibility but no longer meaningful (queue sizing is now an engine concern).

## [0.6.0] - 2026-03-11

### Added

- **Generate Command**: New `aimdb generate` subcommand for data-driven design tooling via `aimdb-codegen`
  - Mermaid diagram generation from `.aimdb/state.toml`
  - Rust schema source generation (`generated_schema.rs`)
  - Common crate scaffolding
  - Hub binary scaffolding
- **Watch Command**: New `aimdb watch` subcommand for live record monitoring
  - Real-time event streaming with formatted output
- New dependency on `aimdb-codegen` for code generation capabilities

## [0.5.0] - 2026-02-21

### Added

- **Graph Commands**: New `aimdb graph` command group for dependency graph exploration
  - `graph nodes`: List all nodes with origin, buffer type, capacity, tap count, outbound status
  - `graph edges`: List all directed edges showing data flow between records
  - `graph order`: Show topological ordering of records (spawn/initialization order)
  - `graph dot`: Export dependency graph in DOT format for Graphviz visualization
- **Graph Output Formatting**: Table and JSON formatters for graph data
  - Color-coded origin types (source=cyan, link=green, transform=yellow)
  - Color-coded edge types (transform_input=blue, tap=gray)
- **README Documentation**: Comprehensive documentation for all graph commands with examples

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-client` and `aimdb-core` dependencies to 0.4.0

## [0.3.0] - 2025-12-15

### Changed

- **RecordMetadata Updates**: Output formatters updated to handle new `record_id` and `record_key` fields in `RecordMetadata`
- JSON and table output now display RecordId and RecordKey alongside TypeId for complete record identification

## [0.1.0] - 2025-11-06

### Added

- Initial release of AimDB CLI tool (skeleton implementation)
- Instance discovery command structure
- Record inspection command structure
- Live watch functionality scaffolding
- Basic command-line argument parsing

### Known Limitations

- Commands are not yet fully implemented
- Placeholder implementation for future development

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.3.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
