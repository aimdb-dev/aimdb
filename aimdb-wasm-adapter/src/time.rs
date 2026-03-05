//! TimeOps implementation for the WASM runtime.
//!
//! Uses `Performance.now()` for high-resolution relative timestamps and
//! `setTimeout` (via a JS Promise) for async sleep.
//!
//! Works in both Window (browser) and Worker/ServiceWorker contexts by
//! accessing `globalThis` via `js_sys::global()` instead of `web_sys::window()`.

use crate::runtime::WasmAdapter;
use aimdb_executor::TimeOps;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// A wrapper that unsafely implements `Send` for a future.
///
/// # Safety
///
/// Only safe on `wasm32-unknown-unknown` where all execution is single-threaded.
/// The inner future will never actually be sent between threads.
pub(crate) struct SendFuture<F>(pub(crate) F);

// SAFETY: wasm32 is single-threaded — the future cannot be sent to another thread
unsafe impl<F> Send for SendFuture<F> {}

impl<F: Future> Future for SendFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We only project to the inner field, preserving Pin guarantees.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        inner.poll(cx)
    }
}

/// Milliseconds since page load (from `Performance.now()`).
///
/// Wraps an `f64` — browser time APIs return floating-point milliseconds.
#[derive(Clone, Debug)]
pub struct WasmInstant(pub(crate) f64);

/// Duration in milliseconds.
#[derive(Clone, Debug)]
pub struct WasmDuration(pub(crate) f64);

// SAFETY: single-threaded wasm32 — no concurrent access possible
unsafe impl Send for WasmInstant {}
unsafe impl Sync for WasmInstant {}
unsafe impl Send for WasmDuration {}
unsafe impl Sync for WasmDuration {}

// ─── globalThis helpers (Window + Worker compatible) ──────────────────────

/// Get `performance.now()` from `globalThis`.
///
/// Works in Window, Worker, and ServiceWorker contexts.
#[cfg(feature = "wasm-runtime")]
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
#[cfg(feature = "wasm-runtime")]
fn global_set_timeout(callback: &js_sys::Function, delay_ms: i32) {
    use wasm_bindgen::JsCast;

    let global = js_sys::global();
    let set_timeout: js_sys::Function = js_sys::Reflect::get(&global, &"setTimeout".into())
        .expect("globalThis.setTimeout not available")
        .unchecked_into();
    let _ = set_timeout.call2(&global, callback, &delay_ms.into());
}

// ─── TimeOps ──────────────────────────────────────────────────────────────

impl TimeOps for WasmAdapter {
    type Instant = WasmInstant;
    type Duration = WasmDuration;

    fn now(&self) -> WasmInstant {
        #[cfg(feature = "wasm-runtime")]
        {
            WasmInstant(global_performance_now())
        }

        #[cfg(not(feature = "wasm-runtime"))]
        {
            // Fallback for native-target unit tests — monotonic counter
            WasmInstant(0.0)
        }
    }

    fn duration_since(&self, later: WasmInstant, earlier: WasmInstant) -> Option<WasmDuration> {
        let diff = later.0 - earlier.0;
        if diff >= 0.0 {
            Some(WasmDuration(diff))
        } else {
            None
        }
    }

    fn millis(&self, ms: u64) -> WasmDuration {
        WasmDuration(ms as f64)
    }

    fn secs(&self, secs: u64) -> WasmDuration {
        WasmDuration(secs as f64 * 1000.0)
    }

    fn micros(&self, micros: u64) -> WasmDuration {
        WasmDuration(micros as f64 / 1000.0)
    }

    fn sleep(&self, duration: WasmDuration) -> impl Future<Output = ()> + Send {
        #[cfg(feature = "wasm-runtime")]
        {
            use futures_util::FutureExt;

            // Convert setTimeout Promise to a Rust Future.
            // setTimeout never rejects, so the Ok/Err result is safe to discard.
            let fut = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                &mut |resolve, _reject| {
                    global_set_timeout(&resolve, duration.0 as i32);
                },
            ))
            .map(|_result| ());

            // SAFETY: wasm32 is single-threaded — JsFuture (which contains Rc<RefCell>)
            // cannot be accessed concurrently. The Send bound is required by the
            // TimeOps trait but is vacuously satisfied on wasm32.
            SendFuture(fut)
        }

        #[cfg(not(feature = "wasm-runtime"))]
        {
            let _ = duration;
            // Fallback for native-target unit tests — resolve immediately
            SendFuture(core::future::ready(()))
        }
    }
}
