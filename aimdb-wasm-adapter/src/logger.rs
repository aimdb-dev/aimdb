//! Logger implementation for the WASM runtime.
//!
//! Maps AimDB log levels to browser console methods:
//! - `info`  → `console.log`
//! - `debug` → `console.debug`
//! - `warn`  → `console.warn`
//! - `error` → `console.error`

use crate::runtime::WasmAdapter;
use aimdb_executor::Logger;

impl Logger for WasmAdapter {
    fn info(&self, message: &str) {
        #[cfg(feature = "wasm-runtime")]
        web_sys::console::log_1(&message.into());

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = message;
        }
    }

    fn debug(&self, message: &str) {
        #[cfg(feature = "wasm-runtime")]
        web_sys::console::debug_1(&message.into());

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = message;
        }
    }

    fn warn(&self, message: &str) {
        #[cfg(feature = "wasm-runtime")]
        web_sys::console::warn_1(&message.into());

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = message;
        }
    }

    fn error(&self, message: &str) {
        #[cfg(feature = "wasm-runtime")]
        web_sys::console::error_1(&message.into());

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = message;
        }
    }
}
