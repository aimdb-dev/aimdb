//! Consumer-facing reader handles.
//!
//! The [`BufferReader`] / [`JsonBufferReader`] SPIs are object-safe and
//! poll-based so adapters can implement them without a per-message
//! `Pin<Box<dyn Future>>` allocation. These handles restore the ergonomic
//! `async fn recv().await` surface for consumers by wrapping the erased
//! reader's `poll_*` method in [`core::future::poll_fn`] — which is `core`-only
//! (no_std-clean), allocation-free, and `unsafe`-free.
//!
//! The wrapped future is `Send` because the boxed reader is `Send`.

use alloc::boxed::Box;
#[cfg(feature = "remote")]
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Context, Poll};

use crate::buffer::BufferReader;
use crate::DbError;

#[cfg(feature = "remote")]
use crate::buffer::JsonBufferReader;

/// Owned, ergonomic handle over an erased [`BufferReader`].
///
/// Returned by `Consumer::subscribe`. This is the "boxed lane": one indirect
/// call per `recv`, zero AimDB-added heap allocations per message. (The generic
/// monomorphized `Reader<T, B>` fast lane remains dormant.)
pub struct Reader<T: Clone + Send> {
    inner: Box<dyn BufferReader<T> + Send>,
}

impl<T: Clone + Send> Reader<T> {
    /// Wrap an erased reader in an ergonomic handle.
    pub fn new(inner: Box<dyn BufferReader<T> + Send>) -> Self {
        Self { inner }
    }

    /// Receive the next value, awaiting until one is available.
    ///
    /// Allocation-free: wraps the erased reader's
    /// [`poll_recv`](BufferReader::poll_recv) via `core::future::poll_fn`.
    ///
    /// # Behavior by Buffer Type
    /// - **SPMC Ring**: Returns next value, or `Lagged(n)` if fell behind
    /// - **SingleLatest**: Waits for value change, returns most recent
    /// - **Mailbox**: Waits for slot value, takes and clears it
    pub async fn recv(&mut self) -> Result<T, DbError> {
        poll_fn(|cx| self.inner.poll_recv(cx)).await
    }

    /// Poll for the next value, for hand-written `poll` code.
    ///
    /// Follows the [`BufferReader::poll_recv`] contract: on `Pending` the
    /// waker in `cx` is registered (the latest one wins), and spurious
    /// wake-ups are allowed. `Pending` does not always mean the buffer is
    /// empty: on Tokio a broadcast reader also returns it, with a self-wake,
    /// once the task's cooperative budget is spent.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        self.inner.poll_recv(cx)
    }

    /// Non-blocking receive — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv(&mut self) -> Result<T, DbError> {
        self.inner.try_recv()
    }
}

/// Owned, ergonomic handle over an erased [`JsonBufferReader`].
///
/// Returned by `subscribe_json`. Awaiting `recv_json` or
/// `recv_json_bytes` wraps the corresponding poll method via
/// `core::future::poll_fn` — no extra per-message future box.
#[cfg(feature = "remote")]
pub struct JsonReader {
    inner: Box<dyn JsonBufferReader + Send>,
}

#[cfg(feature = "remote")]
impl JsonReader {
    /// Wrap an erased JSON reader in an ergonomic handle.
    pub fn new(inner: Box<dyn JsonBufferReader + Send>) -> Self {
        Self { inner }
    }

    /// Receive the next value as JSON, awaiting until one is available.
    pub async fn recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        poll_fn(|cx| self.inner.poll_recv_json(cx)).await
    }

    /// Receive the next value as owned JSON bytes.
    pub async fn recv_json_bytes(&mut self) -> Result<Vec<u8>, DbError> {
        poll_fn(|cx| self.inner.poll_recv_json_bytes(cx)).await
    }

    /// Non-blocking receive as JSON — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        self.inner.try_recv_json()
    }

    /// Non-blocking receive as owned JSON bytes — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv_json_bytes(&mut self) -> Result<Vec<u8>, DbError> {
        self.inner.try_recv_json_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::task::Wake;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use futures_util::task::AtomicWaker;

    /// One-slot buffer: `0` is empty. Its reader registers the waker on
    /// `Pending`, and `produce` wakes it.
    #[derive(Default)]
    struct Slot {
        value: AtomicU32,
        waker: AtomicWaker,
    }

    impl Slot {
        fn produce(&self, v: u32) {
            self.value.store(v, Ordering::Release);
            self.waker.wake();
        }
    }

    struct SlotReader(Arc<Slot>);

    impl BufferReader<u32> for SlotReader {
        fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<u32, DbError>> {
            self.0.waker.register(cx.waker());
            match self.0.value.swap(0, Ordering::AcqRel) {
                0 => Poll::Pending,
                v => Poll::Ready(Ok(v)),
            }
        }

        fn try_recv(&mut self) -> Result<u32, DbError> {
            match self.0.value.swap(0, Ordering::AcqRel) {
                0 => Err(DbError::BufferEmpty),
                v => Ok(v),
            }
        }
    }

    #[derive(Default)]
    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn poll_recv_registers_the_waker_and_is_woken_by_a_produce() {
        let slot = Arc::new(Slot::default());
        let mut reader = Reader::new(Box::new(SlotReader(Arc::clone(&slot))));
        let flag = Arc::new(Flag::default());
        let waker = Arc::clone(&flag).into();
        let mut cx = Context::from_waker(&waker);

        assert!(reader.poll_recv(&mut cx).is_pending());
        assert!(!flag.0.load(Ordering::Acquire));

        slot.produce(7);
        assert!(flag.0.load(Ordering::Acquire));
        assert!(matches!(reader.poll_recv(&mut cx), Poll::Ready(Ok(7))));
    }
}
