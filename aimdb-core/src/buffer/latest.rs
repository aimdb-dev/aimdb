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

#[cfg(feature = "std")]
type Mutex<T> = std::sync::Mutex<T>;
#[cfg(not(feature = "std"))]
type Mutex<T> = spin::Mutex<T>;

#[cfg(feature = "std")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(not(feature = "std"))]
fn lock<T>(m: &Mutex<T>) -> spin::MutexGuard<'_, T> {
    m.lock()
}

/// Wraps a buffer and remembers the last value pushed into it, so `peek()`
/// returns that value for buffers that have no current-value slot of their
/// own.
///
/// The lock is held across the inner push, so with concurrent writers the
/// remembered value is always the one the inner buffer received last.
/// `push` is synchronous and non-blocking on every adapter, so this holds the
/// lock only briefly. On `no_std` the lock is a spin lock: do not push to a
/// wrapped buffer from an interrupt handler that can preempt another pusher.
pub(crate) struct LatestSlot<T: Clone + Send> {
    inner: Arc<dyn DynBuffer<T>>,
    latest: Mutex<Option<T>>,
}

impl<T: Clone + Send> LatestSlot<T> {
    pub(crate) fn new(inner: Arc<dyn DynBuffer<T>>) -> Self {
        Self {
            inner,
            latest: Mutex::new(None),
        }
    }
}

impl<T: Clone + Send + 'static> DynBuffer<T> for LatestSlot<T> {
    fn push(&self, value: T) {
        let mut latest = lock(&self.latest);
        *latest = Some(value.clone());
        self.inner.push(value);
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
        lock(&self.latest).clone()
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
    struct Stream(Mutex<alloc::vec::Vec<u32>>);

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
            lock(&self.0).push(value);
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
        let slot = LatestSlot::new(Arc::new(Stream(Mutex::new(alloc::vec::Vec::new()))));
        assert_eq!(slot.peek(), None);
    }

    #[test]
    fn peek_returns_the_last_push_and_the_inner_buffer_gets_every_push() {
        let inner = Arc::new(Stream(Mutex::new(alloc::vec::Vec::new())));
        let slot = LatestSlot::new(inner.clone() as Arc<dyn DynBuffer<u32>>);
        slot.push(1);
        slot.push(2);
        assert_eq!(slot.peek(), Some(2));
        // peek does not consume.
        assert_eq!(slot.peek(), Some(2));
        assert_eq!(*lock(&inner.0), [1, 2]);
    }

    #[test]
    fn as_any_downcasts_to_the_inner_buffer() {
        let slot = LatestSlot::new(Arc::new(Stream(Mutex::new(alloc::vec::Vec::new()))));
        assert!(slot.as_any().downcast_ref::<Stream>().is_some());
    }
}
