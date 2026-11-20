use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use core::task::Waker;

/// Shared state between [`CancelToken`] and [`CancelHandle`].
struct CancelInner {
    cancelled: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

/// Token held by the subscription task (reader side).
///
/// Polled in a `futures_util::future::select` alongside `reader.recv()`.
/// When [`CancelHandle::cancel()`] fires, the stored waker is woken and
/// `is_cancelled()` returns `true`, causing the select to resolve.
pub(crate) struct CancelToken {
    inner: Rc<CancelInner>,
}

/// Handle held by the JS unsubscribe closure.
///
/// Calling [`cancel()`](CancelHandle::cancel) sets the flag and wakes the
/// subscription task so it exits immediately — even if `recv()` is blocked.
pub(crate) struct CancelHandle {
    inner: Rc<CancelInner>,
}

// SAFETY: wasm32 is single-threaded — no concurrent access possible
unsafe impl Send for CancelToken {}
unsafe impl Sync for CancelToken {}
unsafe impl Send for CancelHandle {}
unsafe impl Sync for CancelHandle {}

/// Create a linked cancel token/handle pair.
pub(crate) fn cancel_pair() -> (CancelToken, CancelHandle) {
    let inner = Rc::new(CancelInner {
        cancelled: Cell::new(false),
        waker: RefCell::new(None),
    });
    (
        CancelToken {
            inner: inner.clone(),
        },
        CancelHandle { inner },
    )
}

impl CancelToken {
    /// Returns `true` if [`CancelHandle::cancel()`] has been called.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.get()
    }

    /// Store the current task's waker so [`CancelHandle::cancel()`] can wake it.
    pub(crate) fn register_waker(&self, waker: &Waker) {
        *self.inner.waker.borrow_mut() = Some(waker.clone());
    }
}

impl CancelHandle {
    /// Signal cancellation and wake the subscription task.
    ///
    /// Idempotent — safe to call multiple times (React StrictMode).
    pub(crate) fn cancel(&self) {
        self.inner.cancelled.set(true);
        if let Some(w) = self.inner.waker.borrow_mut().take() {
            w.wake();
        }
    }
}
