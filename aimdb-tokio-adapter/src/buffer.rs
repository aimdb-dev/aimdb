//! Tokio buffer implementations for AimDB
//!
//! This module provides Tokio-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Tokio's async synchronization primitives:
//!
//! - **SPMC Ring**: `tokio::sync::broadcast` for bounded multi-consumer queues
//! - **SingleLatest**: `tokio::sync::watch` for latest-value semantics
//! - **Mailbox**: `std::sync::Mutex` slot + a hand-rolled waker list for
//!   single-slot overwrite (design 037 / W8 — no `Notify`, no per-message alloc)
//!
//! The broadcast/watch readers are poll-based ([`BufferReader::poll_recv`]) with
//! no per-message heap allocation: each holds a [`ReusableBoxFuture`] that
//! round-trips its receiver (these primitives expose no public poll API), so the
//! single boxed future is allocated once per subscriber and reused per message.

use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll, Waker};

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader};
use aimdb_core::DbError;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::ReusableBoxFuture;

#[cfg(feature = "observability")]
use aimdb_core::buffer::{BufferCounters, BufferMetrics, BufferMetricsSnapshot};

/// Tokio buffer implementation
pub struct TokioBuffer<T: Clone + Send + Sync + 'static> {
    inner: Arc<TokioBufferInner<T>>,
    #[cfg(feature = "observability")]
    metrics: Arc<BufferCounters>,
}

/// Shared state for the Mailbox (single-slot overwrite) buffer.
///
/// Replaces the pre-W8 `Notify` with an explicit waker list beside the slot, so
/// `poll_recv` registers a waker on `Pending` and `push` wakes them — no
/// `Notify` permit subtleties, no per-message allocation (design 037 / W8 §6).
///
/// `pub` only because it appears in the `pub` [`TokioBufferReader`] reader enum;
/// it is an implementation detail and not part of the supported API.
#[doc(hidden)]
pub struct MailboxState<T> {
    /// The single value slot; a new `push` overwrites any unconsumed value.
    slot: Option<T>,
    /// Set when the producer-side [`TokioBuffer`] is dropped (design 039 F6).
    /// A parked reader observing `closed && slot.is_none()` resolves to
    /// `BufferClosed` instead of hanging forever.
    closed: bool,
    /// Parked readers, keyed by a per-reader `waker_key` (design 039 F5) so a
    /// dropped reader can remove exactly its own entry instead of the list
    /// growing unboundedly across repeated subscribe→park→drop cycles.
    wakers: Vec<(usize, Waker)>,
    /// Always empty when not mid-`push`; `push` swaps it with `wakers` so it
    /// can wake outside the lock (design 039 F4) without reallocating
    /// `wakers` from scratch every call — the design doc's own alternative:
    /// "drains into a scratch Vec kept in the state to stay zero-alloc".
    /// Both vecs' capacities stabilize after the first few pushes and are
    /// then just swapped back and forth, preserving W8's
    /// zero-allocs-in-steady-state property that a plain `mem::take` would
    /// break (a freshly-taken empty `Vec` has no capacity, so the next
    /// reader registration would have to reallocate).
    wake_scratch: Vec<(usize, Waker)>,
    /// Monotonic counter handing out the next `waker_key`.
    next_key: usize,
}

/// Internal buffer variants using Tokio primitives
enum TokioBufferInner<T: Clone + Send + Sync + 'static> {
    Broadcast {
        tx: broadcast::Sender<T>,
    },
    Watch {
        tx: watch::Sender<Option<T>>,
    },
    Mailbox {
        state: Arc<StdMutex<MailboxState<T>>>,
    },
}

impl<T: Clone + Send + Sync + 'static> Buffer<T> for TokioBuffer<T> {
    type Reader = TokioBufferReader<T>;

    fn new(cfg: &BufferCfg) -> Self {
        #[cfg(feature = "observability")]
        let capacity = match cfg {
            BufferCfg::SpmcRing { capacity } => *capacity,
            BufferCfg::SingleLatest => 1, // Conceptually holds 1 value
            BufferCfg::Mailbox => 1,      // Single slot
        };

        let inner = match &cfg {
            BufferCfg::SpmcRing { capacity } => {
                let (tx, _) = broadcast::channel(*capacity);
                TokioBufferInner::Broadcast { tx }
            }
            BufferCfg::SingleLatest => {
                let (tx, _rx) = watch::channel(None);
                TokioBufferInner::Watch { tx }
            }
            BufferCfg::Mailbox => TokioBufferInner::Mailbox {
                state: Arc::new(StdMutex::new(MailboxState {
                    slot: None,
                    closed: false,
                    wakers: Vec::new(),
                    wake_scratch: Vec::new(),
                    next_key: 0,
                })),
            },
        };

        Self {
            inner: Arc::new(inner),
            #[cfg(feature = "observability")]
            metrics: Arc::new(BufferCounters::new(capacity)),
        }
    }

    fn push(&self, value: T) {
        #[cfg(feature = "observability")]
        self.metrics.increment_produced();

        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                let _ = tx.send(value);
            }
            TokioBufferInner::Watch { tx } => {
                // send_replace updates the slot unconditionally; send() would
                // fail (and silently drop the value) when no receivers exist,
                // which would also break peek() for producers that publish
                // before any subscriber attaches.
                tx.send_replace(Some(value));
            }
            TokioBufferInner::Mailbox { state } => {
                // Swap `wakers` with the (always-empty-when-idle)
                // `wake_scratch` and let the guard drop *before* waking
                // (design 039 F4) — `Waker::wake()` can run arbitrary
                // callback code (e.g. re-entrant `try_recv()` on this same
                // buffer), which must not happen while this
                // `std::sync::Mutex` is still held. Swapping (not
                // `mem::take`) keeps `wakers`' allocated capacity in place
                // for the next reader registration, and `wake_scratch`'s
                // capacity (from the previous push) is what the wake list
                // below reuses — steady state is zero-alloc either way, per
                // `MailboxState::wake_scratch`'s doc.
                let mut to_wake = {
                    let mut guard = state.lock().unwrap();
                    guard.slot = Some(value);
                    let state = &mut *guard;
                    std::mem::swap(&mut state.wakers, &mut state.wake_scratch);
                    std::mem::take(&mut state.wake_scratch)
                };
                // Wake-all: spurious wakeups are benign — losers re-poll to
                // `Pending` and re-register (design 037 §6). `wake_by_ref`
                // (not `wake`) so `to_wake` can be cleared and recycled
                // below instead of being consumed here.
                for (_, waker) in &to_wake {
                    waker.wake_by_ref();
                }
                // Recycle the drained (capacity-retaining) buffer back into
                // `wake_scratch` for the next push's swap.
                to_wake.clear();
                state.lock().unwrap().wake_scratch = to_wake;
            }
        }
    }

    fn subscribe(&self) -> Self::Reader {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => TokioBufferReader::Broadcast {
                // Allocate the reusable future box once, here, capturing the
                // freshly-subscribed receiver — reused for every message (W8).
                recv: ReusableBoxFuture::new(broadcast_recv(tx.subscribe())),
                #[cfg(feature = "observability")]
                metrics: Arc::clone(&self.metrics),
            },
            TokioBufferInner::Watch { tx } => {
                // D1 (design 039): a fresh subscriber observes the current
                // value once, as if it had been pushed after subscription —
                // matches embassy's native watch::Receiver behavior.
                // `mark_changed()` forces the first `changed()` to fire even
                // if nothing new is pushed; `watch_recv` handles the
                // nothing-pushed-yet case (slot still `None`) by looping
                // instead of returning a spurious empty value.
                let mut rx = tx.subscribe();
                rx.mark_changed();
                TokioBufferReader::Watch {
                    recv: ReusableBoxFuture::new(watch_recv(rx)),
                    #[cfg(feature = "observability")]
                    metrics: Arc::clone(&self.metrics),
                }
            }
            TokioBufferInner::Mailbox { state } => TokioBufferReader::Mailbox {
                state: Arc::clone(state),
                waker_key: None,
                #[cfg(feature = "observability")]
                metrics: Arc::clone(&self.metrics),
            },
        }
    }
}

/// Explicit DynBuffer implementation for TokioBuffer
///
/// When the `observability` feature is enabled, `metrics_snapshot()` returns actual
/// buffer statistics. Otherwise it returns `None`.
impl<T: Clone + Send + Sync + 'static> aimdb_core::buffer::DynBuffer<T> for TokioBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn peek(&self) -> Option<T> {
        match &*self.inner {
            // watch::Sender::borrow() reads the slot non-destructively.
            TokioBufferInner::Watch { tx } => tx.borrow().clone(),
            // Same Mutex the Mailbox buffer already uses for the slot.
            TokioBufferInner::Mailbox { state } => state.lock().unwrap().slot.clone(),
            // broadcast has no canonical latest — see design 031 §SPMC Ring.
            TokioBufferInner::Broadcast { .. } => None,
        }
    }

    #[cfg(feature = "observability")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        Some(<Self as BufferMetrics>::metrics(self))
    }

    #[cfg(feature = "observability")]
    fn reset_metrics(&self) {
        <Self as BufferMetrics>::reset_metrics(self);
    }
}

/// Implementation of BufferMetrics for TokioBuffer (metrics feature only)
#[cfg(feature = "observability")]
impl<T: Clone + Send + Sync + 'static> BufferMetrics for TokioBuffer<T> {
    fn metrics(&self) -> BufferMetricsSnapshot {
        // Calculate current occupancy based on buffer type
        let current_occupancy = match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                // Tokio broadcast::Sender exposes len() for queue length
                tx.len()
            }
            TokioBufferInner::Watch { tx } => {
                // Watch semantically always holds the "latest" value slot.
                // We report 1 if not closed, even if the inner Option<T> is None,
                // because watch channels are designed for "current state" patterns
                // where the slot conceptually always exists.
                if tx.is_closed() {
                    0
                } else {
                    1
                }
            }
            TokioBufferInner::Mailbox { state } => {
                // Lock held only for is_some() check, released immediately.
                if state.lock().unwrap().slot.is_some() {
                    1
                } else {
                    0
                }
            }
        };

        self.metrics
            .snapshot((current_occupancy, self.metrics.capacity()))
    }

    fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

/// Output of the broadcast reader's reusable future: the `recv()` result paired
/// with the receiver handed back so the next future can reuse it.
type BroadcastRecvOutput<T> = (
    Result<T, broadcast::error::RecvError>,
    broadcast::Receiver<T>,
);

/// Output of the watch reader's reusable future. `watch_recv` loops internally
/// past an empty (`None`) slot (design 039 D1/F3), so `Ok` here always carries
/// a real value — no `Option` wrapping needed.
type WatchRecvOutput<T> = (
    Result<T, watch::error::RecvError>,
    watch::Receiver<Option<T>>,
);

/// Await the next broadcast value, returning the receiver for reuse.
///
/// `broadcast::Receiver` exposes no public poll API, so the reader stores this
/// future in a [`ReusableBoxFuture`] and round-trips the receiver through it —
/// one allocation per subscriber, reused for every message (design 037 / W8).
async fn broadcast_recv<T: Clone>(mut rx: broadcast::Receiver<T>) -> BroadcastRecvOutput<T> {
    let res = rx.recv().await;
    (res, rx)
}

/// Await the next watch change, returning the receiver for reuse.
///
/// Uses `borrow_and_update()` (not a bare `borrow()`) so the receiver's "seen"
/// marker advances atomically with the read — a `changed()` that resolves
/// followed by a separate `borrow()` leaves a window where a concurrent `push`
/// can land between the two calls, so the borrow observes a *newer* value than
/// the one that woke `changed()` without clearing its change marker, causing
/// the next `changed()` to fire again for a value already delivered (design
/// 039 F3).
///
/// Loops past an `Ok(())` with an empty (`None`) slot: `subscribe()` calls
/// `mark_changed()` on freshly-subscribed receivers so a late subscriber
/// observes the current value once (design 039 D1); if nothing has been
/// pushed yet, that manufactured "changed" event has no value to deliver, and
/// looping (not erroring) is what makes `poll_recv` correctly resolve to
/// `Pending`/`BufferEmpty` rather than the spurious `BufferClosed` `map_watch`
/// would otherwise report for `Ok(None)`.
async fn watch_recv<T: Clone>(mut rx: watch::Receiver<Option<T>>) -> WatchRecvOutput<T> {
    loop {
        match rx.changed().await {
            Ok(()) => {
                // Bind to an owned value first — the `Ref` guard
                // `borrow_and_update()` returns must drop before `rx` can be
                // moved out in the `return` below.
                let value = rx.borrow_and_update().clone();
                if let Some(v) = value {
                    return (Ok(v), rx);
                }
                // Fake-changed with nothing pushed yet — keep waiting.
            }
            Err(e) => return (Err(e), rx),
        }
    }
}

/// Poll a reusable round-trip future, re-arming it with the returned
/// receiver *before* handing back the result — the shared shape behind
/// `poll_recv`/`try_recv`'s Broadcast and Watch arms (design 039 F14). The
/// re-arm-before-return invariant (losing it drops the receiver and stalls
/// the reader forever) now lives in exactly one place instead of being
/// guarded by convention at 4 call sites.
fn poll_rearm<O, R, F>(
    recv: &mut ReusableBoxFuture<'static, (O, R)>,
    cx: &mut Context<'_>,
    make: fn(R) -> F,
) -> Poll<O>
where
    F: std::future::Future<Output = (O, R)> + Send + 'static,
{
    match recv.poll(cx) {
        Poll::Ready((result, rx)) => {
            recv.set(make(rx));
            Poll::Ready(result)
        }
        Poll::Pending => Poll::Pending,
    }
}

/// Tokio-based buffer reader
pub enum TokioBufferReader<T: Clone + Send + Sync + 'static> {
    Broadcast {
        recv: ReusableBoxFuture<'static, BroadcastRecvOutput<T>>,
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
    Watch {
        recv: ReusableBoxFuture<'static, WatchRecvOutput<T>>,
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
    Mailbox {
        state: Arc<StdMutex<MailboxState<T>>>,
        /// This reader's key into `MailboxState::wakers`, once it has
        /// registered one (design 039 F5). `None` until the first `Pending`
        /// `poll_recv`; used by `Drop` to remove exactly this reader's entry.
        waker_key: Option<usize>,
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
}

impl<T: Clone + Send + Sync + 'static> TokioBufferReader<T> {
    /// Map a broadcast `recv()` result into the AimDB error space (and record
    /// metrics). Shared by `poll_recv` and `try_recv`.
    fn map_broadcast(
        result: Result<T, broadcast::error::RecvError>,
        #[cfg(feature = "observability")] metrics: &BufferCounters,
    ) -> Result<T, DbError> {
        match result {
            Ok(value) => {
                #[cfg(feature = "observability")]
                metrics.increment_consumed();
                Ok(value)
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                #[cfg(feature = "observability")]
                metrics.add_dropped(n);
                Err(DbError::BufferLagged {
                    lag_count: n,
                    buffer_name: "broadcast".to_string(),
                })
            }
            Err(broadcast::error::RecvError::Closed) => Err(DbError::BufferClosed {
                buffer_name: "broadcast".to_string(),
            }),
        }
    }

    /// Map a watch `changed()` result into the AimDB error space.
    fn map_watch(
        result: Result<T, watch::error::RecvError>,
        #[cfg(feature = "observability")] metrics: &BufferCounters,
    ) -> Result<T, DbError> {
        match result {
            Ok(v) => {
                #[cfg(feature = "observability")]
                metrics.increment_consumed();
                Ok(v)
            }
            Err(_) => Err(DbError::BufferClosed {
                buffer_name: "watch".to_string(),
            }),
        }
    }
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioBufferReader<T> {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        match self {
            TokioBufferReader::Broadcast {
                recv,
                #[cfg(feature = "observability")]
                metrics,
            } => match poll_rearm(recv, cx, broadcast_recv) {
                Poll::Ready(result) => Poll::Ready(Self::map_broadcast(
                    result,
                    #[cfg(feature = "observability")]
                    metrics,
                )),
                Poll::Pending => Poll::Pending,
            },
            TokioBufferReader::Watch {
                recv,
                #[cfg(feature = "observability")]
                metrics,
            } => match poll_rearm(recv, cx, watch_recv) {
                Poll::Ready(result) => Poll::Ready(Self::map_watch(
                    result,
                    #[cfg(feature = "observability")]
                    metrics,
                )),
                Poll::Pending => Poll::Pending,
            },
            TokioBufferReader::Mailbox {
                state,
                waker_key,
                #[cfg(feature = "observability")]
                metrics,
            } => {
                let mut guard = state.lock().unwrap();
                if let Some(value) = guard.slot.take() {
                    #[cfg(feature = "observability")]
                    metrics.increment_consumed();
                    Poll::Ready(Ok(value))
                } else if guard.closed {
                    // Producer-side buffer dropped while this reader was
                    // parked (design 039 F6) — resolve instead of hanging.
                    Poll::Ready(Err(DbError::BufferClosed {
                        buffer_name: "mailbox".to_string(),
                    }))
                } else {
                    // Keyed upsert (design 039 F5): replace this reader's own
                    // entry in place if it already registered one, instead of
                    // a `will_wake` scan over every parked reader's waker.
                    //
                    // `push` drains *every* entry out of `wakers` on every
                    // call (`mem::take`, design 039 F4), which invalidates
                    // every outstanding key — including this reader's, if it
                    // was woken by that push and is back here Pending again
                    // with a now-stale `waker_key`. Falling through to
                    // register fresh (instead of silently doing nothing when
                    // the key lookup misses) is what makes this self-healing:
                    // without it, a reader can return `Pending` with no
                    // waker actually registered anywhere, and hang forever.
                    let existing =
                        waker_key.and_then(|key| guard.wakers.iter().position(|(k, _)| *k == key));
                    match existing {
                        Some(idx) => guard.wakers[idx].1 = cx.waker().clone(),
                        None => {
                            let key = guard.next_key;
                            guard.next_key += 1;
                            guard.wakers.push((key, cx.waker().clone()));
                            *waker_key = Some(key);
                        }
                    }
                    Poll::Pending
                }
            }
        }
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        match self {
            // `broadcast`/`watch` have no public poll API, and their receivers
            // live inside the reusable future. Poll that future with a no-op
            // waker: `Ready` means a value/error is available now (try-recv
            // semantics); `Pending` means empty. On `Ready`, re-arm the future.
            TokioBufferReader::Broadcast {
                recv,
                #[cfg(feature = "observability")]
                metrics,
            } => {
                let waker = Waker::noop();
                let mut cx = Context::from_waker(waker);
                match poll_rearm(recv, &mut cx, broadcast_recv) {
                    Poll::Ready(result) => Self::map_broadcast(
                        result,
                        #[cfg(feature = "observability")]
                        metrics,
                    ),
                    Poll::Pending => Err(DbError::BufferEmpty),
                }
            }
            TokioBufferReader::Watch {
                recv,
                #[cfg(feature = "observability")]
                metrics,
            } => {
                let waker = Waker::noop();
                let mut cx = Context::from_waker(waker);
                match poll_rearm(recv, &mut cx, watch_recv) {
                    Poll::Ready(result) => Self::map_watch(
                        result,
                        #[cfg(feature = "observability")]
                        metrics,
                    ),
                    Poll::Pending => Err(DbError::BufferEmpty),
                }
            }
            TokioBufferReader::Mailbox {
                state,
                #[cfg(feature = "observability")]
                metrics,
                ..
            } => {
                let mut guard = state.lock().unwrap();
                match guard.slot.take() {
                    Some(val) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Ok(val)
                    }
                    // Matches how Broadcast/Watch already surface closure on
                    // both APIs (design 039 F6).
                    None if guard.closed => Err(DbError::BufferClosed {
                        buffer_name: "mailbox".to_string(),
                    }),
                    None => Err(DbError::BufferEmpty),
                }
            }
        }
    }
}

/// Removes this reader's own waker entry from `MailboxState` on drop (design
/// 039 F5) — without this, repeated subscribe→park→drop cycles would grow
/// `MailboxState::wakers` unboundedly, since nothing else ever prunes a
/// parked-but-abandoned reader's entry. Broadcast/Watch need no action: their
/// `ReusableBoxFuture` (and the receiver it owns) cleans up on its own drop.
impl<T: Clone + Send + Sync + 'static> Drop for TokioBufferReader<T> {
    fn drop(&mut self) {
        if let TokioBufferReader::Mailbox {
            state,
            waker_key: Some(key),
            ..
        } = self
        {
            state.lock().unwrap().wakers.retain(|(k, _)| k != key);
        }
    }
}

/// Signals Mailbox closure to any parked readers when the producer-side
/// buffer is dropped (design 039 F6) — without this, a reader parked in
/// `poll_recv` (registered a waker, returned `Pending`) would hang forever,
/// since nothing would ever wake it again. Broadcast/Watch need no explicit
/// `Drop`: dropping the last `Sender` already signals closure through tokio's
/// own `Closed` error path (see `map_broadcast`/`map_watch`).
impl<T: Clone + Send + Sync + 'static> Drop for TokioBuffer<T> {
    fn drop(&mut self) {
        if let TokioBufferInner::Mailbox { state } = &*self.inner {
            // Same drain-then-wake-outside-the-lock discipline as `push`
            // (design 039 F4) — do not call `Waker::wake()` while holding
            // this `std::sync::Mutex`.
            let to_wake = {
                let mut guard = state.lock().unwrap();
                guard.closed = true;
                std::mem::take(&mut guard.wakers)
            };
            for (_, waker) in to_wake {
                waker.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::buffer::Reader;

    /// Wrap a concrete `TokioBufferReader` in the ergonomic `Reader<T>` so the
    /// tests can keep exercising `recv().await` / `try_recv()` (design 037 / W8).
    fn rdr<T: Clone + Send + Sync + 'static>(buffer: &TokioBuffer<T>) -> Reader<T> {
        Reader::new(Box::new(buffer.subscribe()))
    }

    #[tokio::test]
    async fn test_spmc_ring_basic() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_spmc_ring_multiple_consumers() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader1 = rdr(&buffer);
        let mut reader2 = rdr(&buffer);
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader1.recv().await.unwrap(), 1);
        assert_eq!(reader2.recv().await.unwrap(), 1);
        assert_eq!(reader1.recv().await.unwrap(), 2);
        assert_eq!(reader2.recv().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_single_latest_basic() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_single_latest_skip_intermediate() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        assert_eq!(reader.recv().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_mailbox_basic() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_mailbox_overwrite() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader.recv().await.unwrap(), 2);
    }

    // ========================================================================
    // Integration Tests - Buffer Semantics
    // ========================================================================

    #[tokio::test]
    async fn test_spmc_ring_overflow_behavior() {
        // Small buffer to test overflow - need to send more than capacity
        // for a slow reader to lag
        let cfg = BufferCfg::SpmcRing { capacity: 3 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = rdr(&buffer);

        // Send more messages than capacity without reading
        // This will cause the slow reader to lag
        for i in 0..10 {
            buffer.push(i);
        }

        // First recv should detect lag
        let result = reader.recv().await;
        match result {
            Err(DbError::BufferLagged { lag_count, .. }) => {
                assert!(lag_count > 0, "Should detect lag: lagged by {}", lag_count);
            }
            Ok(val) => {
                // On fast systems, might get first value before lag
                // Try again - subsequent values should show lag or higher values
                assert!(val >= 0, "Should get a valid value");
            }
            _ => panic!("Unexpected error: {:?}", result),
        }

        // Should be able to continue reading newer values
        // (exact values depend on timing, but should be higher numbers)
        for _ in 0..3 {
            match reader.recv().await {
                Ok(val) => {
                    assert!((0..10).contains(&val), "Should read values in range");
                }
                Err(DbError::BufferLagged { .. }) => {
                    // Also acceptable - means we're catching up
                }
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }
    }

    #[tokio::test]
    async fn test_spmc_ring_multiple_independent_consumers() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Create three independent readers
        let mut reader1 = rdr(&buffer);
        let mut reader2 = rdr(&buffer);
        let mut reader3 = rdr(&buffer);

        // Send values
        for i in 0..5 {
            buffer.push(i);
        }

        // Each reader should receive all values independently
        for expected in 0..5 {
            assert_eq!(reader1.recv().await.unwrap(), expected);
            assert_eq!(reader2.recv().await.unwrap(), expected);
            assert_eq!(reader3.recv().await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn test_single_latest_skips_intermediate_values() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = rdr(&buffer);

        // Send multiple values rapidly
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        buffer.push(4);
        buffer.push(5);

        // Small delay to ensure values are written
        sleep(Duration::from_millis(10)).await;

        // Should only get the latest value
        let value = reader.recv().await.unwrap();
        assert_eq!(value, 5, "Should skip intermediate values and get latest");

        // Wait with no new values - should block/wait
        // (We'll test this with a timeout)
    }

    #[tokio::test]
    async fn test_single_latest_multiple_consumers_all_get_latest() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Create readers BEFORE sending values
        let mut reader1 = rdr(&buffer);
        let mut reader2 = rdr(&buffer);

        // Send values
        buffer.push(10);
        buffer.push(20);
        buffer.push(30);

        sleep(Duration::from_millis(10)).await;

        // Both readers should get the latest value
        assert_eq!(reader1.recv().await.unwrap(), 30);
        assert_eq!(reader2.recv().await.unwrap(), 30);
    }

    #[tokio::test]
    async fn test_mailbox_overwrite_semantics() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = rdr(&buffer);

        // Send first value
        buffer.push(1);

        // Send second value before reading - should overwrite
        buffer.push(2);

        // Send third value - should overwrite again
        buffer.push(3);

        sleep(Duration::from_millis(10)).await;

        // Should only get the last value
        let value = reader.recv().await.unwrap();
        assert_eq!(
            value, 3,
            "Mailbox should overwrite, only latest value available"
        );
    }

    #[tokio::test]
    async fn test_mailbox_single_slot_behavior() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = rdr(&buffer);

        // Send and immediately read
        buffer.push(10);
        sleep(Duration::from_millis(10)).await;
        assert_eq!(reader.recv().await.unwrap(), 10);

        // Send another value
        buffer.push(20);
        sleep(Duration::from_millis(10)).await;
        assert_eq!(reader.recv().await.unwrap(), 20);

        // Verify it's truly single-slot: send multiple, only last survives
        buffer.push(30);
        buffer.push(40);
        buffer.push(50);
        sleep(Duration::from_millis(10)).await;

        let value = reader.recv().await.unwrap();
        assert_eq!(value, 50, "Only the last value should be in the slot");
    }

    // ========================================================================
    // try_recv Tests — Non-blocking receive
    // ========================================================================

    #[tokio::test]
    async fn test_try_recv_broadcast_empty() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // No values written — try_recv returns Empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_try_recv_broadcast_single_value() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        buffer.push(42);
        assert_eq!(reader.try_recv().unwrap(), 42);

        // Buffer is empty again
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_try_recv_broadcast_drains_all_pending() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // Write 5 values
        for i in 0..5 {
            buffer.push(i);
        }

        // Drain all via try_recv
        let mut values = Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => values.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }

        assert_eq!(values, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_try_recv_broadcast_handles_lag() {
        let cfg = BufferCfg::SpmcRing { capacity: 4 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // Write 10 values into capacity-4 ring — reader falls behind
        for i in 0..10 {
            buffer.push(i);
        }

        // First try_recv should return Lagged error
        let first = reader.try_recv();
        assert!(
            matches!(first, Err(DbError::BufferLagged { .. })),
            "Expected BufferLagged, got {:?}",
            first
        );

        // Subsequent calls return the values still in the ring
        let mut values = Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => values.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(DbError::BufferLagged { .. }) => continue,
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }

        // Only the last few values survive in the ring
        assert!(!values.is_empty());
        assert!(values.len() <= 4);
    }

    #[tokio::test]
    async fn test_try_recv_watch_empty() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // No values written — try_recv returns Empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_try_recv_watch_returns_latest() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        // Should return latest value
        let val = reader.try_recv().unwrap();
        assert_eq!(val, 3);

        // No new changes — empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));

        // Push a new value
        buffer.push(4);
        assert_eq!(reader.try_recv().unwrap(), 4);
    }

    // ── F3 / D1 regression tests (design 039) ──────────────────────────────

    #[tokio::test]
    async fn test_watch_try_recv_no_duplicate() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        buffer.push(1);
        assert_eq!(reader.try_recv().unwrap(), 1);
        // A second try_recv with no intervening push must not redeliver the
        // same value — `borrow_and_update()` (not a bare `borrow()`) is what
        // guarantees this.
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_watch_fresh_subscriber_sees_current_value() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Push happens *before* subscribe.
        buffer.push(42);
        let mut reader = rdr(&buffer);

        // D1: a fresh subscriber observes the current value once, without
        // requiring a second push.
        assert_eq!(reader.try_recv().unwrap(), 42);
    }

    #[tokio::test]
    async fn test_watch_fresh_subscriber_no_push_yet_does_not_error() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // `subscribe()` calls `mark_changed()` unconditionally (D1), but with
        // nothing pushed yet that must resolve to "no value", not the
        // spurious `BufferClosed` an unhandled `Ok(None)` would produce.
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));

        buffer.push(7);
        assert_eq!(reader.try_recv().unwrap(), 7);
    }

    #[tokio::test]
    async fn test_watch_fresh_subscriber_no_push_yet_poll_recv_pending() {
        use tokio::time::{timeout, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // `recv().await` (poll_recv under the hood) must stay Pending, not
        // resolve to `Err(BufferClosed)`, for a fresh subscriber with nothing
        // pushed yet.
        assert!(timeout(Duration::from_millis(20), reader.recv())
            .await
            .is_err());

        buffer.push(3);
        assert_eq!(reader.recv().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_try_recv_mailbox_empty() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // No values written — try_recv returns Empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_try_recv_mailbox_takes_value() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        buffer.push(1);
        buffer.push(2); // overwrites

        // Takes the latest value
        let val = reader.try_recv().unwrap();
        assert_eq!(val, 2);

        // Slot is now empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    // ── F4/F5/F6 regression tests (design 039) ──────────────────────────────
    // These reach into `TokioBufferReader::Mailbox`'s private fields directly
    // — allowed since `tests` is a descendant of the `buffer` module that
    // defines them.

    #[test]
    fn test_mailbox_push_does_not_wake_under_lock() {
        use std::task::Wake;

        struct ReentrantWaker {
            state: Arc<StdMutex<MailboxState<i32>>>,
        }
        impl Wake for ReentrantWaker {
            fn wake(self: Arc<Self>) {
                // If `push` still held the Mailbox lock while calling this,
                // `try_lock()` would fail (design 039 F4).
                assert!(
                    self.state.try_lock().is_ok(),
                    "push must not hold the Mailbox lock while waking parked readers"
                );
            }
        }

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();

        let state = match &reader {
            TokioBufferReader::Mailbox { state, .. } => Arc::clone(state),
            _ => unreachable!(),
        };
        let waker = std::task::Waker::from(Arc::new(ReentrantWaker { state }));
        let mut cx = Context::from_waker(&waker);

        // Register the waker via a Pending poll.
        assert!(reader.poll_recv(&mut cx).is_pending());

        // Push wakes the parked reader; the assertion inside `wake()` fails
        // the test if the lock was still held.
        buffer.push(1);
    }

    #[test]
    fn test_mailbox_waker_pruned_on_reader_drop() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Grab a handle to the shared state via one throwaway subscribe (not
        // polled, so it never registers a waker itself).
        let throwaway = buffer.subscribe();
        let state = match &throwaway {
            TokioBufferReader::Mailbox { state, .. } => Arc::clone(state),
            _ => unreachable!(),
        };
        drop(throwaway);

        let waker = Waker::noop();
        for _ in 0..1000 {
            let mut reader = buffer.subscribe();
            let mut cx = Context::from_waker(waker);
            assert!(reader.poll_recv(&mut cx).is_pending());
            drop(reader);
        }

        assert_eq!(
            state.lock().unwrap().wakers.len(),
            0,
            "dropped readers must prune their own waker entry, not leak it"
        );
    }

    #[tokio::test]
    async fn test_mailbox_drop_buffer_wakes_parked_reader_closed() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        let handle = tokio::spawn(async move { reader.recv().await });

        // Give the spawned task a chance to park in poll_recv.
        sleep(Duration::from_millis(10)).await;

        drop(buffer);

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(DbError::BufferClosed { .. })),
            "parked reader must resolve to BufferClosed when the producer drops, not hang"
        );
    }

    #[tokio::test]
    async fn test_mailbox_repeated_recv_cycles_does_not_hang() {
        use tokio::time::{timeout, Duration};

        // Regression for a bug introduced by the F5 keyed-waker rewrite
        // itself: `push` drains *every* entry out of `wakers` on every call
        // (F4's `mem::take`), which invalidates every outstanding key —
        // including the reader that push is about to wake. If that reader's
        // very next `poll_recv` goes `Pending` again before another push,
        // its stale `waker_key` no longer matches anything in `wakers`, and
        // a naive keyed upsert would silently fail to re-register — the
        // reader returns `Pending` with no waker anywhere, and the next
        // `push` has nothing to wake. One push/recv cycle (as in the other
        // Mailbox tests) can't see this; it needs a persistent reader
        // parking and being woken repeatedly, mirroring a real consumer
        // loop (e.g. `AimDbRunner`'s `.tap()` pump).
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        let handle = tokio::spawn(async move {
            for i in 0..50 {
                let v = reader.recv().await.unwrap();
                assert_eq!(v, i);
            }
        });

        for i in 0..50 {
            // Yield so the reader task actually parks (Pending) before each
            // push, exercising the re-registration path every cycle.
            tokio::task::yield_now().await;
            buffer.push(i);
        }

        timeout(Duration::from_secs(5), handle)
            .await
            .expect("reader hung — stale waker_key after push drained wakers")
            .unwrap();
    }

    // ========================================================================
    // try_recv — Drain-Loop Pattern Tests
    // ========================================================================

    #[tokio::test]
    async fn test_try_recv_interleaved_push_and_drain() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // Push 3, drain all
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        let mut batch1 = Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => batch1.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected: {:?}", e),
            }
        }
        assert_eq!(batch1, vec![1, 2, 3]);

        // Push 2 more, drain again
        buffer.push(4);
        buffer.push(5);

        let mut batch2 = Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => batch2.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected: {:?}", e),
            }
        }
        assert_eq!(batch2, vec![4, 5]);

        // No new data — empty
        assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[tokio::test]
    async fn test_try_recv_multiple_independent_readers() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader_a = rdr(&buffer);
        let mut reader_b = rdr(&buffer);

        // Push values
        for i in 0..5 {
            buffer.push(i);
        }

        // Reader A drains all
        let mut a_values = Vec::new();
        loop {
            match reader_a.try_recv() {
                Ok(val) => a_values.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected: {:?}", e),
            }
        }

        // Reader B drains all — independent cursor
        let mut b_values = Vec::new();
        loop {
            match reader_b.try_recv() {
                Ok(val) => b_values.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected: {:?}", e),
            }
        }

        // Both readers should have received the same 5 values independently
        assert_eq!(a_values, vec![0, 1, 2, 3, 4]);
        assert_eq!(b_values, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_try_recv_after_async_recv() {
        let cfg = BufferCfg::SpmcRing { capacity: 16 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = rdr(&buffer);

        // Push 3 values
        buffer.push(10);
        buffer.push(20);
        buffer.push(30);

        // Consume first via async recv()
        let first = reader.recv().await.unwrap();
        assert_eq!(first, 10);

        // Drain remaining via try_recv()
        let mut remaining = Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => remaining.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(e) => panic!("unexpected: {:?}", e),
            }
        }
        assert_eq!(remaining, vec![20, 30]);
    }

    // ========================================================================
    // peek() Tests — non-destructive buffer-native reads (design 031)
    // ========================================================================

    mod peek_tests {
        use super::super::*;
        use super::rdr;
        use aimdb_core::buffer::DynBuffer;

        #[tokio::test]
        async fn test_peek_single_latest_empty() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::SingleLatest);
            assert_eq!(buffer.peek(), None);
        }

        #[tokio::test]
        async fn test_peek_single_latest_returns_latest() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::SingleLatest);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            DynBuffer::push(&buffer, 3);
            assert_eq!(buffer.peek(), Some(3));
        }

        #[tokio::test]
        async fn test_peek_single_latest_is_non_destructive() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::SingleLatest);
            // Subscribe BEFORE push so the receiver's version counter advances
            // on send_replace. (Watch receivers created after a push will only
            // wake on the *next* push — that's the gap peek() exists to fill.)
            let mut reader = rdr(&buffer);
            DynBuffer::push(&buffer, 42);

            // Multiple peeks return the same value.
            assert_eq!(buffer.peek(), Some(42));
            assert_eq!(buffer.peek(), Some(42));

            // Peek did not consume the value from the subscriber's perspective.
            assert_eq!(reader.recv().await.unwrap(), 42);

            // And peek still works after the subscriber received.
            assert_eq!(buffer.peek(), Some(42));
        }

        #[tokio::test]
        async fn test_peek_single_latest_works_without_subscriber() {
            // The exact case the design 031 snapshot was originally added for:
            // a producer pushes before anyone subscribes. peek() must see it.
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::SingleLatest);
            DynBuffer::push(&buffer, 17);
            assert_eq!(buffer.peek(), Some(17));
        }

        #[tokio::test]
        async fn test_peek_mailbox_empty() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::Mailbox);
            assert_eq!(buffer.peek(), None);
        }

        #[tokio::test]
        async fn test_peek_mailbox_returns_pending() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 7);
            assert_eq!(buffer.peek(), Some(7));
        }

        #[tokio::test]
        async fn test_peek_mailbox_drained_after_recv() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 99);
            assert_eq!(buffer.peek(), Some(99));
            // Subscriber takes the slot.
            let mut reader = rdr(&buffer);
            assert_eq!(reader.recv().await.unwrap(), 99);
            // After take(), peek sees the slot is empty.
            assert_eq!(buffer.peek(), None);
        }

        #[tokio::test]
        async fn test_peek_mailbox_reflects_overwrite() {
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::Mailbox);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            assert_eq!(buffer.peek(), Some(2));
        }

        #[tokio::test]
        async fn test_peek_spmc_ring_returns_none() {
            // Broadcast has no canonical latest — see design 031 §SPMC Ring.
            let buffer = TokioBuffer::<i32>::new(&BufferCfg::SpmcRing { capacity: 8 });
            assert_eq!(buffer.peek(), None);
            DynBuffer::push(&buffer, 1);
            DynBuffer::push(&buffer, 2);
            assert_eq!(buffer.peek(), None);
        }
    }

    // ========================================================================
    // Metrics Tests (feature-gated)
    // ========================================================================

    #[cfg(feature = "observability")]
    mod metrics_tests {
        use super::*;

        #[tokio::test]
        async fn test_spmc_ring_produced_count() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);

            // Initial metrics should be zero
            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 0);
            assert_eq!(metrics.consumed_count, 0);
            assert_eq!(metrics.dropped_count, 0);

            // Push some values
            for i in 0..5 {
                buffer.push(i);
            }

            // Check produced count
            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 5);
            assert_eq!(metrics.consumed_count, 0); // No consumers yet
        }

        #[tokio::test]
        async fn test_spmc_ring_consumed_count() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            // Push and consume
            buffer.push(1);
            buffer.push(2);
            buffer.push(3);

            let _ = reader.recv().await.unwrap();
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 3);
            assert_eq!(metrics.consumed_count, 2);
        }

        #[tokio::test]
        async fn test_spmc_ring_dropped_count_on_lag() {
            let cfg = BufferCfg::SpmcRing { capacity: 3 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            // Overfill buffer to cause lag
            for i in 0..10 {
                buffer.push(i);
            }

            // Try to receive - should detect lag
            let result = reader.recv().await;
            if let Err(DbError::BufferLagged { lag_count, .. }) = result {
                // Check dropped count increased
                let metrics = buffer.metrics();
                assert_eq!(
                    metrics.dropped_count, lag_count,
                    "Dropped count should match lag"
                );
            }
            // Note: On very fast systems, lag might not occur
        }

        #[tokio::test]
        async fn test_metrics_reset() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            // Generate some metrics
            buffer.push(1);
            buffer.push(2);
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert!(metrics.produced_count > 0);
            assert!(metrics.consumed_count > 0);

            // Reset
            buffer.reset_metrics();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 0);
            assert_eq!(metrics.consumed_count, 0);
            assert_eq!(metrics.dropped_count, 0);
        }

        #[tokio::test]
        async fn test_watch_buffer_metrics() {
            let cfg = BufferCfg::SingleLatest;
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            buffer.push(1);
            buffer.push(2);
            buffer.push(3);

            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 3);
            assert_eq!(metrics.consumed_count, 1);
            // Watch doesn't drop, it overwrites
            assert_eq!(metrics.dropped_count, 0);
        }

        #[tokio::test]
        async fn test_mailbox_buffer_metrics() {
            let cfg = BufferCfg::Mailbox;
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            buffer.push(1);
            let _ = reader.recv().await.unwrap();
            buffer.push(2);
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 2);
            assert_eq!(metrics.consumed_count, 2);
        }

        #[tokio::test]
        async fn test_dyn_buffer_metrics_snapshot() {
            use aimdb_core::buffer::DynBuffer;

            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);

            // Push some values using Buffer trait explicitly
            Buffer::push(&buffer, 1);
            Buffer::push(&buffer, 2);

            // Access via DynBuffer trait
            let dyn_buffer: &dyn DynBuffer<i32> = &buffer;
            let snapshot = dyn_buffer.metrics_snapshot();

            assert!(snapshot.is_some());
            let snapshot = snapshot.unwrap();
            assert_eq!(snapshot.produced_count, 2);
        }

        #[tokio::test]
        async fn test_try_recv_tracks_consumed_metrics() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            // Push and try_recv
            buffer.push(1);
            buffer.push(2);
            buffer.push(3);

            let _ = reader.try_recv().unwrap();
            let _ = reader.try_recv().unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 3);
            assert_eq!(
                metrics.consumed_count, 2,
                "try_recv should increment consumed_count"
            );
        }

        #[tokio::test]
        async fn test_try_recv_tracks_dropped_on_lag() {
            let cfg = BufferCfg::SpmcRing { capacity: 3 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = rdr(&buffer);

            // Overfill to cause lag
            for i in 0..10 {
                buffer.push(i);
            }

            // try_recv should detect lag and track dropped count
            let result = reader.try_recv();
            if let Err(DbError::BufferLagged { lag_count, .. }) = result {
                let metrics = buffer.metrics();
                assert_eq!(
                    metrics.dropped_count, lag_count,
                    "Dropped count from try_recv should match lag count"
                );
            }
            // Note: On very fast systems, lag might not always occur
        }
    }
}
