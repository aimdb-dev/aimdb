//! WasmAdapter struct and RuntimeAdapter implementation.
//!
//! Single-threaded WASM runtime. After issue #88 there is no `Spawn` impl —
//! all database futures are driven by `AimDbRunner::run()`, which is awaited
//! at the WASM entry point.

use aimdb_executor::RuntimeAdapter;

/// WASM runtime adapter for AimDB.
///
/// Implements the three executor traits required by `aimdb-core`:
/// [`RuntimeAdapter`], [`TimeOps`](crate::time), and [`Logger`](crate::logger).
/// As a zero-sized type with no fields, it auto-derives `Send + Sync`.
#[derive(Clone, Copy, Debug)]
pub struct WasmAdapter;

impl RuntimeAdapter for WasmAdapter {
    fn runtime_name() -> &'static str {
        "wasm"
    }
}
