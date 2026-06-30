//! Allocation counting for B0 benchmarks.
//!
//! Wraps an inner `GlobalAlloc` with atomic counters to measure per-message
//! allocation overhead. `#[global_allocator]` is a per-binary, link-time
//! declaration, so `CountingAllocator` affects only the bench binaries and has
//! zero impact on production crates. It is generic over the inner allocator so
//! an embedded target can swap `System` for `embedded-alloc`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// Total allocation call count (since last [`reset`]).
pub static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Total bytes allocated (since last [`reset`]).
pub static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

/// Wraps an inner `GlobalAlloc`, incrementing [`ALLOC_COUNT`] and
/// [`ALLOC_BYTES`] on every allocation.
pub struct CountingAllocator<A>(pub A);

// SAFETY: we delegate every call to the inner allocator unchanged;
// the only side-effect is the atomic counter updates.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: caller guarantees `layout` is valid; delegated to inner.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller guarantees `ptr` was allocated by this allocator.
        unsafe { self.0.dealloc(ptr, layout) }
    }
}

/// The global allocator used by all bench binaries.
///
/// Applies to every bench binary that links `aimdb-bench` — not to any
/// production crate.
#[global_allocator]
static GLOBAL: CountingAllocator<System> = CountingAllocator(System);

/// Reset both counters to zero.
///
/// Call once after the warmup phase, immediately before the measured window.
#[inline]
pub fn reset() {
    ALLOC_COUNT.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
}

/// Snapshot the current counters.
///
/// Returns `(count, bytes)` — total allocations and total bytes since the
/// last [`reset`].
#[inline]
pub fn snapshot() -> (u64, u64) {
    (
        ALLOC_COUNT.load(Ordering::Relaxed),
        ALLOC_BYTES.load(Ordering::Relaxed),
    )
}
