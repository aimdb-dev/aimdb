//! Shared atomic counter state for buffer introspection (`observability` feature).
//!
//! Embedded in adapter buffers (`TokioBuffer`, `EmbassyBuffer`) so the same
//! `produced`/`consumed`/`dropped` accounting is reused across runtimes.
//! `portable-atomic` keeps this `no_std` and works on targets without native
//! 64-bit atomics (e.g. `thumbv7em`) when the
//! `portable-atomic/{fallback,critical-section}` features are enabled.

use core::sync::atomic::Ordering;
use portable_atomic::AtomicU64;

use super::BufferMetricsSnapshot;

/// Atomic counters shared between a buffer and its readers.
///
/// Updated with `Ordering::Relaxed` — these are diagnostics, not synchronization
/// primitives.
#[derive(Debug)]
pub struct BufferCounters {
    produced: AtomicU64,
    consumed: AtomicU64,
    dropped: AtomicU64,
    capacity: usize,
}

impl BufferCounters {
    /// Creates new counters, remembering the buffer's declared capacity for
    /// inclusion in [`Self::snapshot`] occupancy tuples.
    pub fn new(capacity: usize) -> Self {
        Self {
            produced: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            capacity,
        }
    }

    /// Increments the `produced` counter (call on each successful push).
    pub fn increment_produced(&self) {
        self.produced.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the `consumed` counter (call on each successful recv).
    pub fn increment_consumed(&self) {
        self.consumed.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `count` to the `dropped` counter (call on lag/overflow detection).
    pub fn add_dropped(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Returns the declared buffer capacity passed to [`Self::new`].
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Snapshots the current counter values, attaching the supplied
    /// `(current_items, capacity)` occupancy tuple.
    pub fn snapshot(&self, occupancy: (usize, usize)) -> BufferMetricsSnapshot {
        BufferMetricsSnapshot {
            produced_count: self.produced.load(Ordering::Relaxed),
            consumed_count: self.consumed.load(Ordering::Relaxed),
            dropped_count: self.dropped.load(Ordering::Relaxed),
            occupancy,
        }
    }

    /// Zeroes all counters. Capacity is preserved.
    pub fn reset(&self) {
        self.produced.store(0, Ordering::Relaxed);
        self.consumed.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_state() {
        let c = BufferCounters::new(16);
        let snap = c.snapshot((0, 16));
        assert_eq!(snap.produced_count, 0);
        assert_eq!(snap.consumed_count, 0);
        assert_eq!(snap.dropped_count, 0);
        assert_eq!(snap.occupancy, (0, 16));
        assert_eq!(c.capacity(), 16);
    }

    #[test]
    fn record_one_of_each() {
        let c = BufferCounters::new(4);
        c.increment_produced();
        c.increment_consumed();
        c.add_dropped(3);
        let snap = c.snapshot((1, 4));
        assert_eq!(snap.produced_count, 1);
        assert_eq!(snap.consumed_count, 1);
        assert_eq!(snap.dropped_count, 3);
        assert_eq!(snap.occupancy, (1, 4));
    }

    #[test]
    fn record_many() {
        let c = BufferCounters::new(8);
        for _ in 0..5 {
            c.increment_produced();
        }
        for _ in 0..3 {
            c.increment_consumed();
        }
        c.add_dropped(2);
        c.add_dropped(5);
        let snap = c.snapshot((2, 8));
        assert_eq!(snap.produced_count, 5);
        assert_eq!(snap.consumed_count, 3);
        assert_eq!(snap.dropped_count, 7);
    }

    #[test]
    fn reset_clears_counts_but_not_capacity() {
        let c = BufferCounters::new(32);
        c.increment_produced();
        c.increment_consumed();
        c.add_dropped(9);
        c.reset();
        let snap = c.snapshot((0, 32));
        assert_eq!(snap.produced_count, 0);
        assert_eq!(snap.consumed_count, 0);
        assert_eq!(snap.dropped_count, 0);
        assert_eq!(c.capacity(), 32);
        // Still usable afterwards.
        c.increment_produced();
        assert_eq!(c.snapshot((1, 32)).produced_count, 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn concurrent_increments() {
        use std::sync::Arc;
        use std::thread;

        let c = Arc::new(BufferCounters::new(64));
        let threads = 8;
        let per_thread = 1000u64;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let c = Arc::clone(&c);
                thread::spawn(move || {
                    for _ in 0..per_thread {
                        c.increment_produced();
                        c.increment_consumed();
                        c.add_dropped(1);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let snap = c.snapshot((0, 64));
        assert_eq!(snap.produced_count, threads as u64 * per_thread);
        assert_eq!(snap.consumed_count, threads as u64 * per_thread);
        assert_eq!(snap.dropped_count, threads as u64 * per_thread);
    }
}
