//! Per-record signal-gauge statistics (feature `observability`).
//!
//! Where [`StageMetrics`](crate::profiling::StageMetrics) times *how long* a
//! stage runs, `SignalStats` folds the *domain value* a record carries —
//! `Observable::signal()` fed in via `.observe()` — into last/min/max/count/mean.
//! It surfaces on the same introspection paths (`record.list`/`record.get`,
//! stage profiling) so a temperature's live °C shows up next to its timing.

use core::sync::atomic::Ordering;
use portable_atomic::AtomicU64;

/// Cumulative statistics for one domain signal.
///
/// Values are `f64`; each field is an `AtomicU64` holding the IEEE-754 bits, so
/// updates are lock-free on every target (no `AtomicF64` feature needed).
/// Updated with `Ordering::Relaxed` — these are diagnostics, not synchronization
/// primitives; concurrent `.observe()` taps (should there be more than one) may
/// call [`update`](Self::update) at once.
#[derive(Debug)]
pub struct SignalStats {
    count: AtomicU64,
    last_bits: AtomicU64,
    min_bits: AtomicU64,
    max_bits: AtomicU64,
    sum_bits: AtomicU64,
}

impl Default for SignalStats {
    fn default() -> Self {
        Self {
            count: AtomicU64::new(0),
            last_bits: AtomicU64::new(0),
            // Seeded to ±∞ so the first real sample always wins the min/max CAS.
            min_bits: AtomicU64::new(f64::INFINITY.to_bits()),
            max_bits: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
            sum_bits: AtomicU64::new(0),
        }
    }
}

impl SignalStats {
    /// Creates empty stats (count 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one signal sample.
    pub fn update(&self, value: f64) {
        self.last_bits.store(value.to_bits(), Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        Self::accumulate(&self.sum_bits, |cur| cur + value);
        Self::keep_if(&self.min_bits, value, |value, cur| value < cur);
        Self::keep_if(&self.max_bits, value, |value, cur| value > cur);
    }

    /// CAS loop applying `f` to the stored `f64`.
    fn accumulate(cell: &AtomicU64, f: impl Fn(f64) -> f64) {
        let mut cur = cell.load(Ordering::Relaxed);
        loop {
            let next = f(f64::from_bits(cur)).to_bits();
            match cell.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// CAS loop storing `value` while `keep(value, current)` holds.
    fn keep_if(cell: &AtomicU64, value: f64, keep: impl Fn(f64, f64) -> bool) {
        let mut cur = cell.load(Ordering::Relaxed);
        while keep(value, f64::from_bits(cur)) {
            match cell.compare_exchange_weak(
                cur,
                value.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Number of samples recorded.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Most recent sample (0.0 if none).
    pub fn last(&self) -> f64 {
        f64::from_bits(self.last_bits.load(Ordering::Relaxed))
    }

    /// Smallest sample seen (0.0 if none).
    pub fn min(&self) -> f64 {
        if self.count() == 0 {
            0.0
        } else {
            f64::from_bits(self.min_bits.load(Ordering::Relaxed))
        }
    }

    /// Largest sample seen (0.0 if none).
    pub fn max(&self) -> f64 {
        if self.count() == 0 {
            0.0
        } else {
            f64::from_bits(self.max_bits.load(Ordering::Relaxed))
        }
    }

    /// Mean of all samples (0.0 if none).
    pub fn mean(&self) -> f64 {
        let count = self.count();
        if count == 0 {
            0.0
        } else {
            f64::from_bits(self.sum_bits.load(Ordering::Relaxed)) / count as f64
        }
    }

    /// Clears all statistics back to their initial state.
    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.last_bits.store(0, Ordering::Relaxed);
        self.min_bits
            .store(f64::INFINITY.to_bits(), Ordering::Relaxed);
        self.max_bits
            .store(f64::NEG_INFINITY.to_bits(), Ordering::Relaxed);
        self.sum_bits.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        let s = SignalStats::new();
        assert_eq!(s.count(), 0);
        assert_eq!(s.last(), 0.0);
        assert_eq!(s.min(), 0.0);
        assert_eq!(s.max(), 0.0);
        assert_eq!(s.mean(), 0.0);
    }

    #[test]
    fn tracks_last_min_max_mean() {
        let s = SignalStats::new();
        for v in [10.0, 30.0, 20.0] {
            s.update(v);
        }
        assert_eq!(s.count(), 3);
        assert_eq!(s.last(), 20.0);
        assert_eq!(s.min(), 10.0);
        assert_eq!(s.max(), 30.0);
        assert_eq!(s.mean(), 20.0);
    }

    #[test]
    fn handles_negative_values() {
        let s = SignalStats::new();
        for v in [-5.0, -20.0, -1.0] {
            s.update(v);
        }
        assert_eq!(s.min(), -20.0);
        assert_eq!(s.max(), -1.0);
        assert_eq!(s.last(), -1.0);
    }

    #[test]
    fn reset_clears_then_reusable() {
        let s = SignalStats::new();
        s.update(5.0);
        s.update(7.0);
        s.reset();
        assert_eq!(s.count(), 0);
        assert_eq!(s.min(), 0.0);
        s.update(3.0);
        assert_eq!(s.count(), 1);
        assert_eq!(s.min(), 3.0);
        assert_eq!(s.max(), 3.0);
    }
}
