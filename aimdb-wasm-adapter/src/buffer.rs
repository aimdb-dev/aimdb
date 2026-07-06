//! Single-threaded buffer implementation for the WASM runtime.
//!
//! Uses `Rc<RefCell<…>>` instead of atomics or channels — zero overhead for
//! the browser's single-threaded execution model.
//!
//! All three buffer types are supported:
//! - **SPMC Ring** — bounded `VecDeque` with per-reader cursors
//! - **SingleLatest** — single slot, version-tracked
//! - **Mailbox** — single slot, take-on-read semantics
//!
//! # Safety
//!
//! `WasmBuffer<T>` and `WasmBufferReader<T>` implement `Send + Sync` via
//! `unsafe impl` because `wasm32-unknown-unknown` is single-threaded.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::task::{Context, Poll, Waker};

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader, DynBuffer};
use aimdb_core::DbError;

// ============================================================================
// Buffer
// ============================================================================

/// Single-threaded buffer for the WASM runtime.
///
/// Wraps an `Rc<RefCell<…>>` inner enum that holds the actual buffer state.
/// All three AimDB buffer types (SPMC Ring, SingleLatest, Mailbox) share
/// this outer struct — the variant is determined by [`BufferCfg`] at
/// construction time.
pub struct WasmBuffer<T> {
    inner: Rc<RefCell<WasmBufferInner<T>>>,
}

// SAFETY: wasm32 is single-threaded — Rc<RefCell<…>> cannot be accessed concurrently
unsafe impl<T> Send for WasmBuffer<T> {}
unsafe impl<T> Sync for WasmBuffer<T> {}

/// Internal buffer state — one variant per buffer type.
enum WasmBufferInner<T> {
    /// Bounded ring buffer with independent consumer cursors.
    SpmcRing {
        /// Ring storage (oldest at front, newest at back).
        ring: VecDeque<T>,
        /// Maximum number of items.
        capacity: usize,
        /// Monotonic write counter — each push increments this.
        /// Readers track their own position against this counter.
        write_seq: u64,
        /// Wakers registered by readers waiting for new data.
        wakers: Vec<Waker>,
    },

    /// Only the latest value, skip intermediates.
    SingleLatest {
        /// Current value (None until first push).
        value: Option<T>,
        /// Monotonic version counter — incremented on each push.
        version: u64,
        /// Wakers registered by readers waiting for a new version.
        wakers: Vec<Waker>,
    },

    /// Single slot, overwrite semantics.
    Mailbox {
        /// Current slot value (taken on read).
        slot: Option<T>,
        /// Wakers registered by readers waiting for a value.
        wakers: Vec<Waker>,
    },
}

impl<T: Clone + Send + 'static> Buffer<T> for WasmBuffer<T> {
    type Reader = WasmBufferReader<T>;

    fn new(cfg: &BufferCfg) -> Self {
        let inner = match cfg {
            BufferCfg::SpmcRing { capacity } => WasmBufferInner::SpmcRing {
                ring: VecDeque::with_capacity(*capacity),
                capacity: *capacity,
                write_seq: 0,
                wakers: Vec::new(),
            },
            BufferCfg::SingleLatest => WasmBufferInner::SingleLatest {
                value: None,
                version: 0,
                wakers: Vec::new(),
            },
            BufferCfg::Mailbox => WasmBufferInner::Mailbox {
                slot: None,
                wakers: Vec::new(),
            },
        };

        WasmBuffer {
            inner: Rc::new(RefCell::new(inner)),
        }
    }

    fn push(&self, value: T) {
        let mut inner = self.inner.borrow_mut();
        match &mut *inner {
            WasmBufferInner::SpmcRing {
                ring,
                capacity,
                write_seq,
                wakers,
            } => {
                if ring.len() >= *capacity {
                    ring.pop_front();
                }
                ring.push_back(value);
                *write_seq += 1;
                wake_all(wakers);
            }
            WasmBufferInner::SingleLatest {
                value: slot,
                version,
                wakers,
            } => {
                *slot = Some(value);
                *version += 1;
                wake_all(wakers);
            }
            WasmBufferInner::Mailbox { slot, wakers } => {
                *slot = Some(value);
                wake_all(wakers);
            }
        }
    }

    fn subscribe(&self) -> Self::Reader {
        let inner = self.inner.borrow();
        let state = match &*inner {
            WasmBufferInner::SpmcRing { write_seq, .. } => {
                // New readers start at the current write position (no backfill).
                ReaderState::SpmcRing {
                    read_seq: *write_seq,
                }
            }
            WasmBufferInner::SingleLatest { .. } => {
                // A fresh subscriber observes the current value once, as if it
                // had been pushed after subscription — matches tokio
                // (`mark_changed()`) and embassy's native watch::Receiver
                // behavior. Starting at version 0 (never-seen) makes an
                // already-published value (version >= 1) read as unseen, so
                // `try_recv` delivers it once; with nothing published yet
                // (version == 0) it reads equal and parks.
                ReaderState::SingleLatest {
                    last_seen_version: 0,
                }
            }
            WasmBufferInner::Mailbox { .. } => ReaderState::Mailbox,
        };

        WasmBufferReader {
            buffer: Rc::clone(&self.inner),
            state,
        }
    }
}

/// Explicit DynBuffer implementation so WasmBuffer can be stored as a trait object.
impl<T: Clone + Send + 'static> DynBuffer<T> for WasmBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value);
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn peek(&self) -> Option<T> {
        match &*self.inner.borrow() {
            // The single slot holds the current value non-destructively.
            WasmBufferInner::SingleLatest { value, .. } => value.clone(),
            // The pending slot, cloned without taking it (drained on recv).
            WasmBufferInner::Mailbox { slot, .. } => slot.clone(),
            // No canonical latest — readers consume a stream. Same as the
            // other adapters.
            WasmBufferInner::SpmcRing { .. } => None,
        }
    }
}

// ============================================================================
// Reader
// ============================================================================

/// Single-threaded buffer reader for the WASM runtime.
///
/// Created by [`WasmBuffer::subscribe()`]. Each reader maintains independent
/// state (cursor position, last-seen version) and can advance at its own pace.
pub struct WasmBufferReader<T> {
    buffer: Rc<RefCell<WasmBufferInner<T>>>,
    state: ReaderState,
}

// SAFETY: wasm32 is single-threaded — no concurrent access possible
unsafe impl<T> Send for WasmBufferReader<T> {}
unsafe impl<T> Sync for WasmBufferReader<T> {}

/// Per-reader tracking state.
enum ReaderState {
    /// For SPMC Ring: the sequence number of the next item to read.
    SpmcRing { read_seq: u64 },
    /// For SingleLatest: the version we last observed.
    SingleLatest { last_seen_version: u64 },
    /// For Mailbox: no extra state (take-on-read).
    Mailbox,
}

impl<T: Clone + Send + 'static> BufferReader<T> for WasmBufferReader<T> {
    /// Poll for the next value.
    ///
    /// On each poll:
    /// 1. Try to read a value (non-blocking).
    /// 2. If available, return `Poll::Ready(Ok(value))`.
    /// 3. If not, register the waker and return `Poll::Pending`.
    ///
    /// The waker is woken when [`WasmBuffer::push()`](WasmBuffer) fires. This is
    /// allocation-free — a per-message `Box::pin(WasmRecvFuture { .. })` existed
    /// solely to satisfy the old async trait signature.
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        // Try non-blocking read first
        match self.try_recv() {
            Ok(value) => Poll::Ready(Ok(value)),
            Err(e @ DbError::BufferLagged { .. }) => Poll::Ready(Err(e)),
            Err(DbError::BufferEmpty) => {
                // Register waker so we get woken on next push
                let mut inner = self.buffer.borrow_mut();
                let wakers = match &mut *inner {
                    WasmBufferInner::SpmcRing { wakers, .. } => wakers,
                    WasmBufferInner::SingleLatest { wakers, .. } => wakers,
                    WasmBufferInner::Mailbox { wakers, .. } => wakers,
                };
                // Deduplicate: only add if no existing waker will wake the same task.
                // Prevents unbounded growth when a single reader is polled repeatedly.
                if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                    wakers.push(cx.waker().clone());
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        let mut inner = self.buffer.borrow_mut();
        match (&mut *inner, &mut self.state) {
            (
                WasmBufferInner::SpmcRing {
                    ring, write_seq, ..
                },
                ReaderState::SpmcRing { read_seq },
            ) => {
                if *read_seq >= *write_seq {
                    return Err(DbError::BufferEmpty);
                }
                // Calculate offset into the ring
                let ring_len = ring.len() as u64;
                let oldest_seq = write_seq.saturating_sub(ring_len);

                if *read_seq < oldest_seq {
                    // Reader fell behind — skip to oldest available
                    let lag_count = oldest_seq - *read_seq;
                    *read_seq = oldest_seq;
                    return Err(DbError::BufferLagged {
                        lag_count,
                        buffer_name: alloc::string::String::from("wasm ring"),
                    });
                }

                let offset = (*read_seq - oldest_seq) as usize;
                let value = ring[offset].clone();
                *read_seq += 1;
                Ok(value)
            }
            (
                WasmBufferInner::SingleLatest { value, version, .. },
                ReaderState::SingleLatest { last_seen_version },
            ) => {
                if *version == *last_seen_version {
                    return Err(DbError::BufferEmpty);
                }
                match value {
                    Some(v) => {
                        *last_seen_version = *version;
                        Ok(v.clone())
                    }
                    // Unreachable given the invariants (value is `Some` iff
                    // version >= 1, and version == 0 is caught above), but
                    // kept as a defensive branch — same shape as tokio's
                    // `watch_recv` loop past an empty slot.
                    None => Err(DbError::BufferEmpty),
                }
            }
            (WasmBufferInner::Mailbox { slot, .. }, ReaderState::Mailbox) => {
                slot.take().ok_or(DbError::BufferEmpty)
            }
            _ => unreachable!("reader state mismatch"),
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Wake all registered wakers and clear the list.
fn wake_all(wakers: &mut Vec<Waker>) {
    for waker in wakers.drain(..) {
        waker.wake();
    }
}

// ============================================================================
// Cancellation
// ============================================================================

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

// ============================================================================
// Tests
// ============================================================================
//
// `WasmBuffer` is pure `core` + `alloc` (`Rc<RefCell<…>>`, no `wasm_bindgen` in
// this module), so these run under the host `cargo test` lane — no wasm-pack or
// headless browser needed. They are plain `#[test]` fns (not
// `#[wasm_bindgen_test]`); the browser lane covers the bindings/bridge layer.
#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::buffer::Reader;

    /// Wrap a concrete `WasmBufferReader` in the ergonomic `Reader<T>` so the
    /// tests can use `try_recv()`, mirroring the tokio adapter's `rdr` helper.
    fn rdr<T: Clone + Send + 'static>(buffer: &WasmBuffer<T>) -> Reader<T> {
        Reader::new(Box::new(buffer.subscribe()))
    }

    // ── SingleLatest fresh-subscriber regression tests ──────────────────────
    // Ported from the tokio adapter (`test_watch_fresh_subscriber_*`). Before
    // the design-040 fix these all failed on WASM: a fresh subscriber snapshotted
    // the current version and so never observed a value published before it
    // subscribed.

    #[test]
    fn test_single_latest_fresh_subscriber_sees_current_value() {
        let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);

        // Push happens *before* subscribe.
        Buffer::push(&buffer, 42);
        let mut reader = rdr(&buffer);

        // A fresh subscriber observes the current value once, without
        // requiring a second push — matches tokio/embassy.
        assert_eq!(reader.try_recv().unwrap(), 42);
        // No new push — subsequent reads wait.
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[test]
    fn test_single_latest_fresh_subscriber_no_push_yet_does_not_error() {
        let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
        let mut reader = rdr(&buffer);

        // Nothing pushed yet: the never-seen reader (version 0) reads equal to
        // the buffer's version 0 and resolves to empty, not a spurious value.
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));

        Buffer::push(&buffer, 7);
        assert_eq!(reader.try_recv().unwrap(), 7);
    }

    #[test]
    fn test_single_latest_fresh_subscriber_no_push_yet_poll_recv_pending() {
        let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
        // Use the concrete reader so we can poll directly with a noop waker —
        // the single-threaded equivalent of tokio's `timeout(recv)` check,
        // with no executor needed.
        let mut reader = buffer.subscribe();

        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        // A fresh subscriber with nothing pushed yet must park, not resolve to
        // an error.
        assert!(reader.poll_recv(&mut cx).is_pending());

        Buffer::push(&buffer, 3);
        assert!(matches!(reader.poll_recv(&mut cx), Poll::Ready(Ok(3))));
    }

    // ── Shared cross-runtime buffer contract ────────────────────────────────
    // The same `aimdb-core` suite the tokio and embassy adapters run, driven on
    // the host via `futures::executor::block_on`. Gated off `wasm32`: the
    // `futures` executor is a host-only dev-dependency and this proves parity on
    // the host lane, while the browser lane covers the bindings/bridge layer.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn buffer_contract_suite() {
        use aimdb_core::buffer::test_support;
        use futures::executor::block_on;

        block_on(test_support::assert_single_latest_contract(|| {
            WasmBuffer::<i32>::new(&BufferCfg::SingleLatest)
        }));
        block_on(test_support::assert_mailbox_contract(|| {
            WasmBuffer::<i32>::new(&BufferCfg::Mailbox)
        }));
        block_on(test_support::assert_spmc_ring_contract(|| {
            WasmBuffer::<i32>::new(&BufferCfg::SpmcRing { capacity: 4 })
        }));
    }

    // ========================================================================
    // peek() Tests — non-destructive buffer-native reads
    //
    // Mirrors the tokio adapter's `peek_tests`; before design 040 WASM had no
    // `peek()` override at all, so AimX `record.get` returned `None` on WASM.
    // ========================================================================

    mod peek_tests {
        use super::super::*;
        use super::rdr;
        use aimdb_core::buffer::DynBuffer;

        #[test]
        fn test_peek_single_latest_empty() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
            assert_eq!(buffer.peek(), None);
        }

        #[test]
        fn test_peek_single_latest_returns_latest() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            DynBuffer::push(&buffer, 3);
            assert_eq!(buffer.peek(), Some(3));
        }

        #[test]
        fn test_peek_single_latest_is_non_destructive() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
            let mut reader = rdr(&buffer);
            DynBuffer::push(&buffer, 42);

            // Multiple peeks return the same value.
            assert_eq!(buffer.peek(), Some(42));
            assert_eq!(buffer.peek(), Some(42));

            // Peek did not consume the value from the subscriber's perspective.
            assert_eq!(reader.try_recv().unwrap(), 42);

            // And peek still works after the subscriber received.
            assert_eq!(buffer.peek(), Some(42));
        }

        #[test]
        fn test_peek_single_latest_works_without_subscriber() {
            // A producer pushes before anyone subscribes. peek() must see it.
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::SingleLatest);
            DynBuffer::push(&buffer, 17);
            assert_eq!(buffer.peek(), Some(17));
        }

        #[test]
        fn test_peek_mailbox_empty() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::Mailbox);
            assert_eq!(buffer.peek(), None);
        }

        #[test]
        fn test_peek_mailbox_returns_pending() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 7);
            assert_eq!(buffer.peek(), Some(7));
            // Non-destructive: the slot is still occupied.
            assert_eq!(buffer.peek(), Some(7));
        }

        #[test]
        fn test_peek_mailbox_drained_after_recv() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 99);
            assert_eq!(buffer.peek(), Some(99));
            // Subscriber takes the slot.
            let mut reader = rdr(&buffer);
            assert_eq!(reader.try_recv().unwrap(), 99);
            // After take(), peek sees the slot is empty.
            assert_eq!(buffer.peek(), None);
        }

        #[test]
        fn test_peek_mailbox_reflects_overwrite() {
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            assert_eq!(buffer.peek(), Some(2));
        }

        #[test]
        fn test_peek_spmc_ring_returns_none() {
            // SpmcRing has no canonical latest.
            let buffer = WasmBuffer::<i32>::new(&BufferCfg::SpmcRing { capacity: 8 });
            assert_eq!(buffer.peek(), None);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            assert_eq!(buffer.peek(), None);
        }
    }
}
