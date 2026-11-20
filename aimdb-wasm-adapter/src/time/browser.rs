use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// A wrapper that implements `Send` for a future **on wasm32 only**, so a
/// JS-touching (`!Send`) future can satisfy the engine's `Send` bounds and box
/// into the `dyn Future + Send` transport shapes.
///
/// # Safety
///
/// The blanket `unsafe impl Send for SendFuture<F>` is sound only where
/// execution is single-threaded. The wrapper is available only on wasm32,
/// and the crate root rejects targets that advertise atomics.
pub(crate) struct SendFuture<F>(pub(crate) F);

// SAFETY: single-threaded wasm32 (without atomics) — the future is never
// actually sent between threads. Arch-gated so it can't satisfy a `Send` bound
// on a multi-threaded native target.
unsafe impl<F> Send for SendFuture<F> {}

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
fn global_set_timeout(callback: &js_sys::Function, delay_ms: i32) {
    use wasm_bindgen::JsCast;

    let global = js_sys::global();
    let set_timeout: js_sys::Function = js_sys::Reflect::get(&global, &"setTimeout".into())
        .expect("globalThis.setTimeout not available")
        .unchecked_into();
    let _ = set_timeout.call2(&global, callback, &delay_ms.into());
}

// ─── Instance clock ───────────────────────────────────────────────────────

/// The instance clock (design 058 §3.2): the realm's `performance.now()`,
/// measured from `build()`.
pub(super) struct WasmClock {
    /// `performance.now()` at `build()`, in milliseconds.
    origin_ms: f64,
}

impl WasmClock {
    pub(super) fn new() -> Self {
        let origin_ms = global_performance_now();
        Self { origin_ms }
    }
}

impl aimdb_core::InstanceClock for WasmClock {
    fn now(&self) -> aimdb_core::Stamp {
        aimdb_core::clock::stamp_from_millis_f64(global_performance_now() - self.origin_ms)
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        // `Date.now()` is wall-clock milliseconds since the Unix epoch.
        let ms = js_sys::Date::now();
        if ms <= 0.0 {
            return None;
        }
        let secs = (ms / 1000.0) as u64;
        let sub_nanos = ((ms % 1000.0) * 1_000_000.0) as u32;
        Some((secs, sub_nanos))
    }
}

pub(super) fn sleep(d: core::time::Duration) -> aimdb_core::BoxFuture {
    use futures_util::FutureExt;

    let ms = d.as_secs_f64() * 1000.0;
    // Convert setTimeout Promise to a Rust Future.
    // setTimeout never rejects, so the Ok/Err result is safe to discard.
    let fut =
        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |resolve, _reject| {
            global_set_timeout(&resolve, ms as i32);
        }))
        .map(|_result| ());
    // SAFETY: wasm32 without atomics is single-threaded — JsFuture
    // (which contains Rc<RefCell>) cannot be accessed concurrently, so
    // the required Send bound is vacuously satisfied.
    alloc::boxed::Box::pin(SendFuture(fut))
}

pub(super) fn log(level: aimdb_core::LogLevel, msg: &str) {
    use aimdb_core::LogLevel;
    match level {
        LogLevel::Debug => web_sys::console::debug_1(&msg.into()),
        LogLevel::Info => web_sys::console::log_1(&msg.into()),
        LogLevel::Warn => web_sys::console::warn_1(&msg.into()),
        LogLevel::Error => web_sys::console::error_1(&msg.into()),
    }
}
