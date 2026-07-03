//! Automatic stage profiling (feature `observability`).
//!
//! AimDB owns the execution boundary for user callbacks (`.source()`, `.tap()`,
//! `.link()`), so it can measure their wall-clock execution time without any
//! user instrumentation:
//!
//! * **source** — wall-clock interval between successive `Producer::produce()` calls.
//! * **tap / link** — wall-clock interval from a buffer read yielding a value to
//!   the next read (≈ the user's per-value processing time).
//!
//! Timing uses the runtime's own clock ([`crate::executor::RuntimeOps`]), so the
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
use core::sync::atomic::Ordering;
use core::task::{Context, Poll};
use portable_atomic::AtomicU64;

use crate::buffer::BufferReader;
use crate::DbError;

/// A monotonic-ish wall clock that returns nanoseconds since an arbitrary epoch.
///
/// Built once per record at spawn time from the runtime adapter and shared
/// (cheaply, behind `Arc`) with the per-stage instrumentation.
pub(crate) type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Builds a [`Clock`] from the dyn-safe runtime capabilities.
pub(crate) fn make_clock(rt: Arc<dyn crate::executor::RuntimeOps>) -> Clock {
    let epoch = rt.now_nanos();
    Arc::new(move || rt.now_nanos().saturating_sub(epoch))
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
    /// Wall-clock (ns) of the first poll of the current recv cycle — the moment
    /// the consumer asked for the next value. Memoized across re-polls so a
    /// `Pending` wait for the producer is not counted as consumer processing
    /// time; cleared when the cycle completes (see `poll_recv`/`try_recv`).
    ///
    /// Residual gap (design 039 F2): if `poll_recv` returns `Pending` and the
    /// caller's future is dropped (cancelled) with no intervening yield, this
    /// timestamp is *not* cleared and gets reused by the next `poll_recv` —
    /// "pending since first ask" is still a defensible interpretation in that
    /// case (the consumer never yielded in between). A precise fix would need
    /// a cancellation hook on the `BufferReader` SPI, rejected as growing the
    /// trait for one wrapper. What *is* fixed: a `try_recv` that completes the
    /// cycle, or a stale value that predates the last completed yield, no
    /// longer bleeds into the next cycle's sample.
    pending_since: Option<u64>,
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
            pending_since: None,
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
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        // `started_ns` ≈ the moment the consumer finished processing the
        // previous value and asked for the next one — i.e. the *first* poll of
        // this recv cycle. Memoized in `pending_since` so re-polls after a
        // `Pending` (waiting on the producer) reuse it instead of resampling;
        // this keeps the recorded interval equal to consumer processing time and
        // matches the prior await-based `recv()`, which captured `started_ns`
        // once when the future was first polled. (Clock is read once per cycle,
        // not once per poll.)
        //
        // A memoized `pending_since` older than `last_yield_ns` belongs to a
        // cycle a `try_recv` already closed out (design 039 F2) — resample
        // instead of reusing it.
        let started_ns = match self.pending_since {
            Some(t) if self.last_yield_ns.is_none_or(|ly| t >= ly) => t,
            _ => {
                let now = (self.clock)();
                self.pending_since = Some(now);
                now
            }
        };
        let result = self.inner.poll_recv(cx);
        if result.is_ready() {
            // The recv "future" completed (Ok or Err) — close out the cycle so
            // the next ask resamples the clock.
            self.pending_since = None;
            if matches!(result, Poll::Ready(Ok(_))) {
                self.on_yield(started_ns);
            }
        }
        result
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        let started_ns = (self.clock)();
        let result = self.inner.try_recv();
        if result.is_ok() {
            // A completed receive ends the cycle regardless of which API
            // completed it — clear so a subsequent `poll_recv` resamples
            // rather than reusing a `pending_since` from before this
            // try_recv (design 039 F2).
            self.pending_since = None;
            self.on_yield(started_ns);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::Waker;

    /// A single-slot inner reader for exercising `ProfilingBufferReader`
    /// without a real buffer/executor. `NO_PREV` (already used elsewhere in
    /// this module as an empty sentinel) doubles as "nothing ready yet".
    struct StepReader {
        slot: Arc<AtomicU64>,
    }

    impl BufferReader<u32> for StepReader {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<u32, DbError>> {
            match self.slot.swap(NO_PREV, Ordering::AcqRel) {
                NO_PREV => Poll::Pending,
                v => Poll::Ready(Ok(v as u32)),
            }
        }

        fn try_recv(&mut self) -> Result<u32, DbError> {
            match self.slot.swap(NO_PREV, Ordering::AcqRel) {
                NO_PREV => Err(DbError::BufferEmpty),
                v => Ok(v as u32),
            }
        }
    }

    fn mock_clock() -> (Clock, Arc<AtomicU64>) {
        let t = Arc::new(AtomicU64::new(0));
        let clock_t = Arc::clone(&t);
        (Arc::new(move || clock_t.load(Ordering::Relaxed)), t)
    }

    #[test]
    fn cancelled_poll_then_try_recv_does_not_leak_into_next_sample() {
        let (clock, t) = mock_clock();
        let metrics = Arc::new(StageMetrics::new());
        let slot = Arc::new(AtomicU64::new(NO_PREV));
        let inner = StepReader {
            slot: Arc::clone(&slot),
        };
        let mut reader: ProfilingBufferReader<u32> =
            ProfilingBufferReader::new(Box::new(inner), Arc::clone(&metrics), clock);

        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        // 1. First poll of the very first cycle: nothing ready yet -> Pending.
        // Memoizes `pending_since = Some(0)`.
        assert!(reader.poll_recv(&mut cx).is_pending());
        assert_eq!(reader.pending_since, Some(0));

        // 2. Simulate cancellation: the caller drops this poll_recv future
        // and never polls it again. Time passes.
        t.store(1_000_000, Ordering::Relaxed);

        // 3. The consumer switches to try_recv instead, and a value is now
        // available. Before the F2 fix, try_recv never cleared
        // `pending_since`, so the stale `Some(0)` would bleed into the next
        // cycle's sample.
        slot.store(7, Ordering::Relaxed);
        assert_eq!(reader.try_recv().unwrap(), 7);
        assert_eq!(
            reader.pending_since, None,
            "try_recv must clear pending_since on Ok"
        );

        // 4. Next cycle: poll again. Since `pending_since` was cleared, this
        // resamples fresh instead of reusing the stale t=0 from step 1.
        t.store(1_100_000, Ordering::Relaxed);
        assert!(reader.poll_recv(&mut cx).is_pending());
        assert_eq!(
            reader.pending_since,
            Some(1_100_000),
            "poll_recv must resample, not reuse the stale pre-cancellation timestamp"
        );

        // 5. Complete this cycle. The recorded sample must be the real
        // processing interval for *this* cycle (start of cycle 4 to end of
        // cycle 3), not the bogus interval back to step 1's t=0 (which would
        // saturate to 0 — the pre-fix bug) and not zero.
        t.store(1_150_000, Ordering::Relaxed);
        slot.store(9, Ordering::Relaxed);
        match reader.poll_recv(&mut cx) {
            Poll::Ready(Ok(v)) => assert_eq!(v, 9),
            other => panic!("expected Ready(Ok(9)), got {other:?}"),
        }
        assert_eq!(metrics.call_count(), 1);
        assert_eq!(metrics.total_time_ns(), 100_000);
    }
}
