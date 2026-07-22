//! RuntimeOps implementation for the WASM runtime.
//!
//! Uses `Performance.now()` for high-resolution relative timestamps,
//! `Date.now()` for wall-clock time, `setTimeout` (via a JS Promise) for
//! async sleep, and the browser console for logging.
//!
//! Works in both Window (browser) and Worker/ServiceWorker contexts by
//! accessing `globalThis` via `js_sys::global()` instead of `web_sys::window()`.

use crate::runtime::WasmAdapter;
#[cfg(feature = "wasm-runtime")]
use core::future::Future;
#[cfg(feature = "wasm-runtime")]
use core::pin::Pin;
#[cfg(feature = "wasm-runtime")]
use core::task::{Context, Poll};

/// A wrapper that unconditionally implements `Send` for a future, so a
/// JS-touching (`!Send`) future can satisfy the engine's `Send` bounds and box
/// into the `dyn Future + Send` transport shapes.
///
/// # Safety
///
/// `wasm-runtime` is a wasm32-only feature in practice: the JS-driven bridge it
/// gates (`ws_bridge`) holds `Rc`/`Closure` state that is genuinely `!Send`, so
/// the crate only compiles for `wasm32-unknown-unknown` once the feature is on
/// (the native `cargo test` lane runs `--no-default-features`, which compiles
/// this type and the bridge out entirely). That leaves
/// `wasm32-unknown-unknown` without the `atomics` / shared-memory proposal as
/// the only configuration where the `Send` impl is live — single-threaded by
/// construction, so the inner future is never actually sent between threads.
/// The `compile_error!` guard below is the backstop: enabling wasm threads
/// invalidates that reasoning and fails the build rather than silently
/// producing UB.
#[cfg(feature = "wasm-runtime")]
pub(crate) struct SendFuture<F>(pub(crate) F);

// Guard: detect wasm32 + threads (atomics target feature). The shared-memory
// proposal makes wasm multi-threaded, which invalidates the Send blanket impl.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
compile_error!(
    "SendFuture's blanket `impl Send` is unsound with wasm threads enabled. \
     Disable the `atomics` target feature or provide a thread-safe implementation."
);

// SAFETY: see the type's docs — with `wasm-runtime` on, the only target this
// compiles for is wasm32 (without atomics), which is single-threaded.
#[cfg(feature = "wasm-runtime")]
unsafe impl<F> Send for SendFuture<F> {}

#[cfg(feature = "wasm-runtime")]
impl<F: Future> Future for SendFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We only project to the inner field, preserving Pin guarantees.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        inner.poll(cx)
    }
}

// ─── globalThis helpers (Window + Worker compatible) ──────────────────────

/// Get `performance.now()` from `globalThis`.
///
/// Works in Window, Worker, and ServiceWorker contexts.
#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
fn global_performance_now() -> f64 {
    use wasm_bindgen::JsCast;

    let global = js_sys::global();
    let perf = js_sys::Reflect::get(&global, &"performance".into())
        .expect("globalThis.performance not available");
    let now = js_sys::Reflect::get(&perf, &"now".into())
        .expect("globalThis.performance.now not available");
    let now_fn: js_sys::Function = now.unchecked_into();
    now_fn
        .call0(&perf)
        .expect("performance.now() call failed")
        .as_f64()
        .expect("performance.now() did not return a number")
}

/// Call `globalThis.setTimeout(callback, delay)`.
///
/// Works in Window, Worker, and ServiceWorker contexts.
#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
fn global_set_timeout(callback: &js_sys::Function, delay_ms: i32) {
    use wasm_bindgen::JsCast;

    let global = js_sys::global();
    let set_timeout: js_sys::Function = js_sys::Reflect::get(&global, &"setTimeout".into())
        .expect("globalThis.setTimeout not available")
        .unchecked_into();
    let _ = set_timeout.call2(&global, callback, &delay_ms.into());
}

// ─── RuntimeOps (dyn-safe capability surface) ─────────────────────────────

impl aimdb_core::RuntimeOps for WasmAdapter {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn now_nanos(&self) -> u64 {
        // `Performance.now()` is monotonic milliseconds since page load.
        #[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
        {
            let nanos = global_performance_now().max(0.0) * 1_000_000.0;
            if nanos >= u64::MAX as f64 {
                u64::MAX
            } else {
                nanos as u64
            }
        }

        #[cfg(not(all(feature = "wasm-runtime", target_arch = "wasm32")))]
        {
            // Fallback for native-target unit tests — clock pinned at 0
            0
        }
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        #[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
        {
            // `Date.now()` is wall-clock milliseconds since the Unix epoch.
            let ms = js_sys::Date::now();
            if ms <= 0.0 {
                return None;
            }
            let secs = (ms / 1000.0) as u64;
            let sub_nanos = ((ms % 1000.0) * 1_000_000.0) as u32;
            Some((secs, sub_nanos))
        }

        #[cfg(not(all(feature = "wasm-runtime", target_arch = "wasm32")))]
        {
            None
        }
    }

    fn sleep(&self, d: core::time::Duration) -> aimdb_core::BoxFuture {
        extern crate alloc;
        let ms = d.as_secs_f64() * 1000.0;

        #[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
        {
            use futures_util::FutureExt;
            // Convert setTimeout Promise to a Rust Future.
            // setTimeout never rejects, so the Ok/Err result is safe to discard.
            let fut = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                &mut |resolve, _reject| {
                    global_set_timeout(&resolve, ms as i32);
                },
            ))
            .map(|_result| ());
            // SAFETY: wasm32 without atomics is single-threaded — JsFuture
            // (which contains Rc<RefCell>) cannot be accessed concurrently, so
            // the required Send bound is vacuously satisfied.
            alloc::boxed::Box::pin(SendFuture(fut))
        }

        #[cfg(not(all(feature = "wasm-runtime", target_arch = "wasm32")))]
        {
            let _ = ms;
            alloc::boxed::Box::pin(core::future::ready(()))
        }
    }

    fn log(&self, level: aimdb_core::LogLevel, msg: &str) {
        #[cfg(feature = "wasm-runtime")]
        {
            use aimdb_core::LogLevel;
            match level {
                LogLevel::Debug => web_sys::console::debug_1(&msg.into()),
                LogLevel::Info => web_sys::console::log_1(&msg.into()),
                LogLevel::Warn => web_sys::console::warn_1(&msg.into()),
                LogLevel::Error => web_sys::console::error_1(&msg.into()),
            }
        }

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = (level, msg);
        }
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
        let t0 = ops.now_nanos();
        assert!(ops.now_nanos() >= t0);
        assert_eq!(ops.unix_time(), None);
    }
}
