# Changelog

All notable changes to `aimdb-codegen` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Generated join handler stubs** updated to match the new task-model `on_triggers` API (Design 027). Multi-input task handlers are now generated as:
  ```rust
  pub async fn task_handler(
      mut _rx: aimdb_core::transform::JoinEventRx,
      _producer: aimdb_core::Producer<Output, TokioAdapter>,
  ) {
      while let Ok(_trigger) = _rx.recv().await {
          todo!("implement task_handler")
      }
  }
  ```
  Previously generated `fn task_handler(JoinTrigger, &mut (), &Producer<...>) -> Pin<Box<dyn Future>>` for the callback model.
- `build_transform_call` for join tasks now emits `.on_triggers(handler)` instead of `.with_state(()).on_trigger(handler)`.

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
