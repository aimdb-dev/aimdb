//! `LatestSlot<T>` — gives any buffer a canonical latest value.
//!
//! A [`SpmcRing`](super::BufferCfg::SpmcRing) is a stream: runtime adapters
//! build it on a broadcast channel, which has no slot to
//! [`peek`](DynBuffer::peek). Rather than make every adapter keep one, core
//! wraps a ring's buffer in a `LatestSlot` when the database is built. The
//! wrapper remembers the last value pushed and answers `peek()` from it;
//! everything else goes straight to the adapter's buffer.
//!
//! Every write reaches the buffer through
//! [`RecordWriter::push`](super::RecordWriter), so the slot sees every value.

use alloc::{boxed::Box, sync::Arc};

use super::traits::{BufferReader, DynBuffer};

#[cfg(feature = "observability")]
use super::BufferMetricsSnapshot;

/// The lock around the remembered value.
///
/// - `std`: `std::sync::Mutex`.
/// - `no_std` + feature `critical-section`: a `critical_section::Mutex`, the
///   same synchronization Embassy's own buffers use. Taking it masks
///   interrupts, so an interrupt handler can never find it held, and pushing to
///   a ring from an interrupt is safe.
/// - `no_std` without it: a spin lock. Correct where nothing preempts a holder
///   (a cooperative executor, wasm), but an interrupt that pushes while the
///   code it interrupted holds the lock would spin forever.
#[cfg(feature = "std")]
struct Slot<T>(std::sync::Mutex<Option<T>>);

#[cfg(feature = "std")]
impl<T> Slot<T> {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<T>) -> R) -> R {
        // A poisoned slot still holds a whole value: `push` only replaces it.
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }
}

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
struct Slot<T>(critical_section::Mutex<core::cell::RefCell<Option<T>>>);

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
impl<T> Slot<T> {
    const fn new() -> Self {
        Self(critical_section::Mutex::new(core::cell::RefCell::new(None)))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<T>) -> R) -> R {
        // The critical section is not re-entered from inside `f` (it only
        // clones a value or pushes to the inner buffer, which takes its own,
        // nestable critical section), so the `RefCell` borrow cannot clash.
        critical_section::with(|cs| f(&mut self.0.borrow_ref_mut(cs)))
    }
}

#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
struct Slot<T>(spin::Mutex<Option<T>>);

#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
impl<T> Slot<T> {
    const fn new() -> Self {
        Self(spin::Mutex::new(None))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<T>) -> R) -> R {
        f(&mut self.0.lock())
    }
}

/// Wraps a buffer and remembers the last value pushed into it, so `peek()`
/// returns that value for buffers that have no current-value slot of their
/// own.
///
/// The lock is held across the inner push, so with concurrent writers the
/// remembered value is always the one the inner buffer received last. Core
/// does not enforce a single producer per ring: several `Producer`s, inbound
/// connectors and `produce()` calls can all write. `push` is synchronous and
/// non-blocking on every adapter, so the lock is held only for one clone and
/// one push. See [`Slot`] for which lock is used where, and when pushing from
/// an interrupt handler is safe.
pub(crate) struct LatestSlot<T: Clone + Send> {
    inner: Arc<dyn DynBuffer<T>>,
    latest: Slot<T>,
}

impl<T: Clone + Send> LatestSlot<T> {
    pub(crate) fn new(inner: Arc<dyn DynBuffer<T>>) -> Self {
        Self {
            inner,
            latest: Slot::new(),
        }
    }
}

impl<T: Clone + Send + 'static> DynBuffer<T> for LatestSlot<T> {
    fn push(&self, value: T) {
        self.latest.with(|latest| {
            *latest = Some(value.clone());
            self.inner.push(value);
        });
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        self.inner.subscribe_boxed()
    }

    /// The adapter's buffer, so downcasts to a concrete buffer type still
    /// work through the wrapper.
    fn as_any(&self) -> &dyn core::any::Any {
        self.inner.as_any()
    }

    fn peek(&self) -> Option<T> {
        self.latest.with(|latest| latest.clone())
    }

    #[cfg(feature = "observability")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        self.inner.metrics_snapshot()
    }

    #[cfg(feature = "observability")]
    fn reset_metrics(&self) {
        self.inner.reset_metrics();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbError;
    use core::task::{Context, Poll};

    /// A buffer with no current-value slot that records what it received.
    struct Stream(Slot<alloc::vec::Vec<u32>>);

    struct NoReader;
    impl BufferReader<u32> for NoReader {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<u32, DbError>> {
            Poll::Pending
        }
        fn try_recv(&mut self) -> Result<u32, DbError> {
            Err(DbError::BufferEmpty)
        }
    }

    impl DynBuffer<u32> for Stream {
        fn push(&self, value: u32) {
            self.0
                .with(|v| v.get_or_insert_with(alloc::vec::Vec::new).push(value));
        }
        fn subscribe_boxed(&self) -> Box<dyn BufferReader<u32> + Send> {
            Box::new(NoReader)
        }
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    #[test]
    fn peek_is_none_before_the_first_push() {
        let slot = LatestSlot::new(Arc::new(Stream(Slot::new())));
        assert_eq!(slot.peek(), None);
    }

    #[test]
    fn peek_returns_the_last_push_and_the_inner_buffer_gets_every_push() {
        let inner = Arc::new(Stream(Slot::new()));
        let slot = LatestSlot::new(inner.clone() as Arc<dyn DynBuffer<u32>>);
        slot.push(1);
        slot.push(2);
        assert_eq!(slot.peek(), Some(2));
        // peek does not consume.
        assert_eq!(slot.peek(), Some(2));
        assert_eq!(inner.0.with(|v| v.clone()), Some(alloc::vec![1, 2]));
    }

    /// With `critical-section`, the slot's lock is a critical section, which
    /// nests: pushing or peeking from code that already masks interrupts (an
    /// interrupt handler, or an inner buffer taking its own critical section)
    /// does not deadlock.
    #[cfg(all(not(feature = "std"), feature = "critical-section"))]
    #[test]
    fn push_and_peek_nest_inside_a_critical_section() {
        let slot = LatestSlot::new(Arc::new(Stream(Slot::new())));
        critical_section::with(|_| {
            slot.push(7);
            assert_eq!(slot.peek(), Some(7));
        });
        assert_eq!(slot.peek(), Some(7));
    }

    #[test]
    fn as_any_downcasts_to_the_inner_buffer() {
        let slot = LatestSlot::new(Arc::new(Stream(Slot::new())));
        assert!(slot.as_any().downcast_ref::<Stream>().is_some());
    }
}
