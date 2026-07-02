//! WasmAdapter struct.
//!
//! Single-threaded WASM runtime. After issue #88 there is no `Spawn` impl —
//! all database futures are driven by `AimDbRunner::run()`, which is awaited
//! at the WASM entry point.

/// WASM runtime adapter for AimDB.
///
/// Implements [`aimdb_core::RuntimeOps`] (see [`crate::time`]) — the one
/// trait a runtime adapter provides. As a zero-sized type with no fields, it
/// auto-derives `Send + Sync`.
#[derive(Clone, Copy, Debug)]
pub struct WasmAdapter;
