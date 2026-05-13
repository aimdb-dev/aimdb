//! Per-stage timing metrics.

use core::sync::atomic::Ordering;
use portable_atomic::AtomicU64;

/// Sentinel used for `min_time_ns` before any sample has been recorded.
const NO_MIN: u64 = u64::MAX;

/// Cumulative timing statistics for a single execution stage
/// (one `.source()`, `.tap()`, `.link()`, or future `.transform()`).
///
/// All times are wall-clock nanoseconds. Updated with `Ordering::Relaxed` — these
/// are diagnostics, not synchronization primitives. Multiple producers may call
/// [`record`](Self::record) concurrently (e.g. cloned `Producer` handles).
#[derive(Debug)]
pub struct StageMetrics {
    call_count: AtomicU64,
    total_time_ns: AtomicU64,
    min_time_ns: AtomicU64,
    max_time_ns: AtomicU64,
}

impl Default for StageMetrics {
    fn default() -> Self {
        Self {
            call_count: AtomicU64::new(0),
            total_time_ns: AtomicU64::new(0),
            min_time_ns: AtomicU64::new(NO_MIN),
            max_time_ns: AtomicU64::new(0),
        }
    }
}

impl StageMetrics {
    /// Creates an empty `StageMetrics`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one stage invocation that took `nanos` nanoseconds of wall-clock time.
    pub fn record(&self, nanos: u64) {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        self.total_time_ns.fetch_add(nanos, Ordering::Relaxed);
        self.min_time_ns.fetch_min(nanos, Ordering::Relaxed);
        self.max_time_ns.fetch_max(nanos, Ordering::Relaxed);
    }

    /// Number of recorded invocations.
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// Cumulative wall-clock time across all invocations, in nanoseconds.
    pub fn total_time_ns(&self) -> u64 {
        self.total_time_ns.load(Ordering::Relaxed)
    }

    /// Mean wall-clock time per invocation, in nanoseconds (0 if no samples).
    pub fn avg_time_ns(&self) -> u64 {
        let count = self.call_count();
        self.total_time_ns().checked_div(count).unwrap_or(0)
    }

    /// Fastest recorded invocation, in nanoseconds (0 if no samples).
    pub fn min_time_ns(&self) -> u64 {
        match self.min_time_ns.load(Ordering::Relaxed) {
            NO_MIN => 0,
            v => v,
        }
    }

    /// Slowest recorded invocation, in nanoseconds (0 if no samples).
    pub fn max_time_ns(&self) -> u64 {
        self.max_time_ns.load(Ordering::Relaxed)
    }

    /// Clears all counters back to their initial state.
    pub fn reset(&self) {
        self.call_count.store(0, Ordering::Relaxed);
        self.total_time_ns.store(0, Ordering::Relaxed);
        self.min_time_ns.store(NO_MIN, Ordering::Relaxed);
        self.max_time_ns.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_call_accessors_are_zero() {
        let m = StageMetrics::new();
        assert_eq!(m.call_count(), 0);
        assert_eq!(m.total_time_ns(), 0);
        assert_eq!(m.avg_time_ns(), 0);
        assert_eq!(m.min_time_ns(), 0);
        assert_eq!(m.max_time_ns(), 0);
    }

    #[test]
    fn record_one() {
        let m = StageMetrics::new();
        m.record(42);
        assert_eq!(m.call_count(), 1);
        assert_eq!(m.total_time_ns(), 42);
        assert_eq!(m.avg_time_ns(), 42);
        assert_eq!(m.min_time_ns(), 42);
        assert_eq!(m.max_time_ns(), 42);
    }

    #[test]
    fn record_many_avg_min_max() {
        let m = StageMetrics::new();
        for v in [10u64, 30, 20] {
            m.record(v);
        }
        assert_eq!(m.call_count(), 3);
        assert_eq!(m.total_time_ns(), 60);
        assert_eq!(m.avg_time_ns(), 20);
        assert_eq!(m.min_time_ns(), 10);
        assert_eq!(m.max_time_ns(), 30);
    }

    #[test]
    fn reset_clears_everything() {
        let m = StageMetrics::new();
        m.record(5);
        m.record(7);
        m.reset();
        assert_eq!(m.call_count(), 0);
        assert_eq!(m.total_time_ns(), 0);
        assert_eq!(m.avg_time_ns(), 0);
        assert_eq!(m.min_time_ns(), 0);
        assert_eq!(m.max_time_ns(), 0);
        // and still usable afterwards
        m.record(3);
        assert_eq!(m.call_count(), 1);
        assert_eq!(m.min_time_ns(), 3);
    }

    #[cfg(feature = "std")]
    #[test]
    fn concurrent_record() {
        use std::sync::Arc;
        use std::thread;

        let m = Arc::new(StageMetrics::new());
        let threads = 8;
        let per_thread = 1000u64;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    for _ in 0..per_thread {
                        m.record(2);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.call_count(), threads as u64 * per_thread);
        assert_eq!(m.total_time_ns(), threads as u64 * per_thread * 2);
        assert_eq!(m.min_time_ns(), 2);
        assert_eq!(m.max_time_ns(), 2);
    }
}
