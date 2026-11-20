//! RuntimeOps implementation for the WASM runtime.
//!
//! Uses `Performance.now()` for high-resolution relative timestamps,
//! `Date.now()` for wall-clock time, `setTimeout` (via a JS Promise) for
//! async sleep, and the browser console for logging.
//!
//! Works in both Window (browser) and Worker/ServiceWorker contexts by
//! accessing `globalThis` via `js_sys::global()` instead of `web_sys::window()`.

use crate::runtime::WasmAdapter;
#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
#[path = "time/browser.rs"]
pub(crate) mod platform;
#[cfg(not(all(feature = "wasm-runtime", target_arch = "wasm32")))]
#[path = "time/fallback.rs"]
mod platform;

// ─── RuntimeOps (dyn-safe capability surface) ─────────────────────────────

impl aimdb_core::RuntimeOps for WasmAdapter {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn sleep(&self, d: core::time::Duration) -> aimdb_core::BoxFuture {
        platform::sleep(d)
    }

    fn log(&self, level: aimdb_core::LogLevel, msg: &str) {
        platform::log(level, msg)
    }

    fn new_clock(&self) -> alloc::sync::Arc<dyn aimdb_core::InstanceClock> {
        alloc::sync::Arc::new(platform::WasmClock::new())
    }
}

#[cfg(test)]
mod runtime_ops_tests {
    use super::*;
    use aimdb_core::RuntimeOps;
    use alloc::sync::Arc;

    // Full async contract needs the browser event loop (setTimeout sleep).
    #[cfg(target_arch = "wasm32")]
    mod wasm {
        use super::*;
        use wasm_bindgen_test::wasm_bindgen_test;

        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

        #[wasm_bindgen_test]
        async fn runtime_ops_contract() {
            let ops: Arc<dyn RuntimeOps> = Arc::new(WasmAdapter);
            aimdb_core::executor::test_support::assert_runtime_ops_contract(ops.as_ref()).await;
        }
    }

    // Native fallback build: cover the sync surface and the dyn coercion.
    // `log` is excluded — the wasm console backend panics off-target
    // (pre-existing); the browser test covers it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn runtime_ops_sync_surface() {
        let ops: Arc<dyn RuntimeOps> = Arc::new(WasmAdapter);
        assert_eq!(ops.name(), "wasm");
        let clock = ops.new_clock();
        let s0 = clock.now();
        assert!(clock.now() >= s0);
        assert_eq!(clock.unix_time(), None);
    }
}
