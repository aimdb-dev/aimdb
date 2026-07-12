# Changelog

All notable changes to `aimdb-codegen` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Issue #177 — generated Postcard records now have a true into-slice codec.**
  Their `Linkable` implementation advertises a 256-byte reusable scratch
  capacity and overrides `encode_into` with `postcard::to_slice`; generated
  outbound connector chains install `with_serializer_into`. Values that exceed
  the scratch capacity retain the existing `postcard::to_allocvec` fallback.
  Host behavior, generated connector wiring, `no_std` Cortex-M compilation, and
  the JSON-free dependency graph are covered by `make codegen-drift`.
- Postcard codegen is now exercised end to end by `make codegen-drift`: a generated Postcard-only common crate roundtrips on the host, cross-compiles for `thumbv7em-none-eabihf`, and proves its normal dependency graph is JSON-free. A generated mixed JSON + Postcard crate is also compiled so codec feature/dependency drift fails in CI (#155).

### Fixed

- Generated `Cargo.toml` for manifests mixing `serialization = "json"` and `serialization = "postcard"` records omitted the `serde_json` dependency (postcard presence suppressed it), so the generated crate failed to compile under the `std` feature. `serde_json` is now emitted whenever any record uses JSON serialization, independent of postcard (#155).

### Changed (breaking)

- Emitted task scaffolds now use `Producer<T>` / `Consumer<T>` (no `, TokioAdapter` second parameter) and emitted doc tables show the same form, matching the M14 cleanup in `aimdb-core` (Design 029). Regenerate downstream scaffolds after upgrading.
- Emitted `configure_schema` signature changed from `<R: Spawn + 'static>` to `<R: RuntimeAdapter + 'static>`; emitted prelude now imports `aimdb_executor::RuntimeAdapter` instead of `Spawn` (Issue #88). Regenerate downstream schemas.

## [0.2.0] - 2026-05-22

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
