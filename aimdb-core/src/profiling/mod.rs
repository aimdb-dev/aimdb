//! Automatic stage profiling (feature `profiling`).
//!
//! AimDB owns the execution boundary for user callbacks (`.source()`, `.tap()`,
//! `.link()`), so it can measure their wall-clock execution time without any
//! user instrumentation:
//!
//! * **source** — wall-clock interval between successive `Producer::produce()` calls.
//! * **tap / link** — wall-clock interval from a buffer read yielding a value to
//!   the next read (≈ the user's per-value processing time).
//!
//! Timing uses the runtime's own clock ([`aimdb_executor::TimeOps`]), so the
//! feature works on `no_std` targets too (it only needs heap + a clock).
//!
//! Measured time is **wall-clock**, including `.await` points / I/O / sleeps — it
//! answers *which* stage is slow, not *why*. Use `tracing` / `tokio-console` for
//! CPU-vs-I/O analysis.

mod info;
mod record_profiling;
mod stage_metrics;

pub use info::StageProfilingInfo;
pub use record_profiling::{RecordProfilingMetrics, StageEntry};
pub use stage_metrics::StageMetrics;

use alloc::{boxed::Box, sync::Arc};
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::Ordering;
use portable_atomic::AtomicU64;

use crate::buffer::BufferReader;
use crate::DbError;

/// A monotonic-ish wall clock that returns nanoseconds since an arbitrary epoch.
///
/// Built once per record at spawn time from the runtime adapter and shared
/// (cheaply, behind `Arc`) with the per-stage instrumentation.
pub(crate) type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Builds a [`Clock`] from a runtime adapter.
pub(crate) fn make_clock<R: aimdb_executor::TimeOps>(rt: Arc<R>) -> Clock {
    let epoch = rt.now();
    Arc::new(move || {
        let now = rt.now();
        match rt.duration_since(now, epoch.clone()) {
            Some(d) => rt.duration_as_nanos(d),
            None => 0,
        }
    })
}

/// Sentinel for "no previous `produce()` recorded yet".
const NO_PREV: u64 = u64::MAX;

/// Per-`Producer` profiling state for a `.source()` stage.
///
/// Shared (behind `Arc`) by all clones of the `Producer`, so the "time since the
/// last produce" cursor is consistent regardless of which clone produced.
pub(crate) struct ProducerProfilingState {
    metrics: Arc<StageMetrics>,
    clock: Clock,
    last_produce_ns: AtomicU64,
}

impl ProducerProfilingState {
    pub(crate) fn new(metrics: Arc<StageMetrics>, clock: Clock) -> Self {
        Self {
            metrics,
            clock,
            last_produce_ns: AtomicU64::new(NO_PREV),
        }
    }

    /// Records one source iteration: the interval since the previous produce.
    /// The first call only seeds the cursor (no sample).
    pub(crate) fn record_produce(&self) {
        let now = (self.clock)();
        let prev = self.last_produce_ns.swap(now, Ordering::Relaxed);
        if prev != NO_PREV {
            self.metrics.record(now.saturating_sub(prev));
        }
    }
}

/// Wraps a [`BufferReader`] to time how long the consumer spends between reads
/// (≈ per-value processing time for a `.tap()` / `.link()` stage).
pub(crate) struct ProfilingBufferReader<T: Clone + Send> {
    inner: Box<dyn BufferReader<T> + Send>,
    metrics: Arc<StageMetrics>,
    clock: Clock,
    /// Wall-clock (ns) at which the last value was handed to the consumer.
    last_yield_ns: Option<u64>,
}

impl<T: Clone + Send> ProfilingBufferReader<T> {
    pub(crate) fn new(
        inner: Box<dyn BufferReader<T> + Send>,
        metrics: Arc<StageMetrics>,
        clock: Clock,
    ) -> Self {
        Self {
            inner,
            metrics,
            clock,
            last_yield_ns: None,
        }
    }

    fn on_yield(&mut self, started_ns: u64) {
        if let Some(prev) = self.last_yield_ns {
            self.metrics.record(started_ns.saturating_sub(prev));
        }
        self.last_yield_ns = Some((self.clock)());
    }
}

impl<T: Clone + Send> BufferReader<T> for ProfilingBufferReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(async move {
            // `started_ns` ≈ the moment the consumer finished processing the
            // previous value and asked for the next one.
            let started_ns = (self.clock)();
            let result = self.inner.recv().await;
            if result.is_ok() {
                self.on_yield(started_ns);
            }
            result
        })
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        let started_ns = (self.clock)();
        let result = self.inner.try_recv();
        if result.is_ok() {
            self.on_yield(started_ns);
        }
        result
    }
}
