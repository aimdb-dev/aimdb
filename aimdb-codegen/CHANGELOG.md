# Changelog

All notable changes to `aimdb-codegen` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [0.1.0] - 2026-03-11

### Added

- Initial release of the AimDB code generation library
- `ArchitectureState` type for reading `.aimdb/state.toml` decision records
- Mermaid diagram generation from architecture state (`generate_mermaid`)
- Rust source generation from architecture state (`generate_rust`)
  - Value structs, key enums, `SchemaType`/`Linkable` implementations
  - `configure_schema()` function scaffolding
  - Common crate, hub crate, and binary crate generation
- State validation module for architecture integrity checks (`validate`)
- TOML serialization/deserialization of architecture state
