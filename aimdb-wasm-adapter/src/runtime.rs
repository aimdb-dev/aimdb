//! WasmAdapter struct and RuntimeAdapter + Spawn implementations
//!
//! Single-threaded WASM runtime — tasks are spawned onto the browser's
//! microtask queue via `wasm_bindgen_futures::spawn_local`.

use aimdb_executor::{ExecutorResult, RuntimeAdapter, Spawn};
use core::future::Future;

/// WASM runtime adapter for AimDB.
///
/// Implements the four executor traits required by `aimdb-core`:
/// [`RuntimeAdapter`], [`Spawn`], [`TimeOps`](crate::time), and
/// [`Logger`](crate::logger).
///
/// # Safety
///
/// `WasmAdapter` implements `Send + Sync` via `unsafe impl` because
/// `wasm32-unknown-unknown` is single-threaded — no concurrent access
/// is possible. This is the identical pattern used by `EmbassyAdapter`.
#[derive(Clone, Copy, Debug)]
pub struct WasmAdapter;

// SAFETY: wasm32-unknown-unknown is single-threaded.
// No concurrent access is possible — Send + Sync are trivially satisfied.
// This is the same pattern as aimdb-embassy-adapter/src/runtime.rs.
unsafe impl Send for WasmAdapter {}
unsafe impl Sync for WasmAdapter {}

impl RuntimeAdapter for WasmAdapter {
    fn runtime_name() -> &'static str {
        "wasm"
    }
}

impl Spawn for WasmAdapter {
    type SpawnToken = (); // Same as Embassy — no join handle

    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // spawn_local requires F: 'static but not F: Send.
        // The Send bound on the trait is satisfied vacuously —
        // all types are effectively Send in a single-threaded context.
        #[cfg(feature = "wasm-runtime")]
        {
            wasm_bindgen_futures::spawn_local(future);
        }

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = future;
            // Without wasm-runtime, we can't spawn — this path is only
            // hit during native-target unit tests.
            return Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                message: "wasm-runtime feature not enabled",
            });
        }

        Ok(())
    }
}
