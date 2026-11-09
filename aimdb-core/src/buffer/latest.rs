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

// ── Contract ────────────────────────────────────────────────────────────────
//
// Code outside core runs during `push` and `peek` at exactly these points:
//
//   (W) wakers: the adapter's push wakes readers synchronously, inside the
//       push call (tokio after releasing its own locks, Embassy while still
//       holding its lock), so a waker registered via `Reader::poll_recv` runs
//       in the middle of `LatestSlot::push`;
//   (C) `T::clone`: the remembered copy, and every `peek`;
//   (D) `T::drop`: the replaced snapshot, and `peek`'s temporary `Arc`;
//   (I) interrupts, on Embassy: they can preempt any of the above.
//
// Invariants, each covered by a test (here or in
// `test_support::assert_latest_slot_contract`, which every adapter runs
// against its real ring):
//
//   1. Ordered: after every push has returned, `peek` returns the value the
//      ring received last, also with concurrent writers.
//   2. The snapshot lock is never held while code outside core runs: not
//      across (W), (C) or (D). So from any of them, `peek` on the same ring
//      returns without deadlock or panic.
//   3. A waker may push to the same ring (W re-entering `push`). On std the
//      writer lock is re-entrant on its own thread; with `critical-section`
//      the critical section nests. (Embassy's own channel panics on a
//      re-entrant publish; that is the adapter's limit, not this wrapper's.)
//   4. With `critical-section`, interrupts cannot find either lock held (I).
//
// Not supported: two threads whose wakers push into each other's rings at the
// same moment (an A→B / B→A lock order across two writer locks). Invariant 1
// needs writers serialized across the adapter's push, and tokio and Embassy
// both wake inside it, so any lock that keeps the order is held during (W).

/// Serializes writers of one ring across the adapter's push (invariant 1),
/// re-entrantly (invariant 3).
#[cfg(feature = "std")]
struct WriterLock {
    lock: std::sync::Mutex<()>,
    /// The thread holding `lock`, as [`thread_token`], or 0.
    owner: core::sync::atomic::AtomicUsize,
}

/// A non-zero id unique among live threads: the address of a thread-local.
#[cfg(feature = "std")]
fn thread_token() -> usize {
    std::thread_local!(static TOKEN: u8 = const { 0 });
    TOKEN.with(|t| t as *const u8 as usize)
}

#[cfg(feature = "std")]
impl WriterLock {
    const fn new() -> Self {
        Self {
            lock: std::sync::Mutex::new(()),
            owner: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        use core::sync::atomic::Ordering::Relaxed;
        let me = thread_token();
        // Only this thread ever stores `me`, and it stores 0 again before
        // unlocking, so reading `me` means this thread holds the lock: a
        // waker re-entering push on the thread that is pushing.
        if self.owner.load(Relaxed) == me {
            return f();
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.owner.store(me, Relaxed);
        struct Release<'a>(&'a core::sync::atomic::AtomicUsize);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(0, Relaxed);
            }
        }
        let _release = Release(&self.owner);
        f()
    }
}

/// With `critical-section`, the critical section itself serializes writers:
/// it masks interrupts and, on multi-core targets, excludes the other cores.
/// It nests, so it is re-entrant.
#[cfg(all(not(feature = "std"), feature = "critical-section"))]
struct WriterLock;

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
impl WriterLock {
    const fn new() -> Self {
        Self
    }
    fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        critical_section::with(|_| f())
    }
}

/// no_std without `critical-section`: nothing can preempt a push (a
/// cooperative executor, wasm), so writers never overlap and there is nothing
/// to serialize. Pushing from an interrupt here is unsupported; enable
/// `critical-section`.
#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
struct WriterLock;

#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
impl WriterLock {
    const fn new() -> Self {
        Self
    }
    fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
}

/// The remembered value. Held as an `Arc` so the lock only ever moves or
/// copies a pointer: `T::clone` and `T::drop` run after it is released
/// (invariant 2).
///
/// - `std`: `std::sync::Mutex`.
/// - `no_std` + `critical-section`: a `critical_section::Mutex` (invariant 4).
/// - `no_std` otherwise: a spin lock.
#[cfg(feature = "std")]
struct Snapshot<T>(std::sync::Mutex<Option<Arc<T>>>);

#[cfg(feature = "std")]
impl<T> Snapshot<T> {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<Arc<T>>) -> R) -> R {
        // Only pointer moves happen under the lock, so it cannot be poisoned
        // mid-update; recover anyway rather than propagate a panic.
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }
}

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
struct Snapshot<T>(critical_section::Mutex<core::cell::RefCell<Option<Arc<T>>>>);

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
impl<T> Snapshot<T> {
    const fn new() -> Self {
        Self(critical_section::Mutex::new(core::cell::RefCell::new(None)))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<Arc<T>>) -> R) -> R {
        // `f` only moves pointers and never re-enters, so the borrow is free.
        critical_section::with(|cs| f(&mut self.0.borrow_ref_mut(cs)))
    }
}

#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
struct Snapshot<T>(spin::Mutex<Option<Arc<T>>>);

#[cfg(all(not(feature = "std"), not(feature = "critical-section")))]
impl<T> Snapshot<T> {
    const fn new() -> Self {
        Self(spin::Mutex::new(None))
    }
    fn with<R>(&self, f: impl FnOnce(&mut Option<Arc<T>>) -> R) -> R {
        f(&mut self.0.lock())
    }
}

/// Wraps a buffer and remembers the last value pushed into it, so `peek()`
/// returns that value for buffers that have no current-value slot of their
/// own. See the contract at the top of this module.
///
/// Cost per push: one `T::clone` and one `Arc` allocation.
pub(crate) struct LatestSlot<T: Clone + Send> {
    inner: Arc<dyn DynBuffer<T>>,
    writer: WriterLock,
    latest: Snapshot<T>,
}

impl<T: Clone + Send> LatestSlot<T> {
    pub(crate) fn new(inner: Arc<dyn DynBuffer<T>>) -> Self {
        Self {
            inner,
            writer: WriterLock::new(),
            latest: Snapshot::new(),
        }
    }
}

impl<T: Clone + Send + Sync + 'static> DynBuffer<T> for LatestSlot<T> {
    fn push(&self, value: T) {
        // (C) outside every lock.
        let snapshot = Arc::new(value.clone());
        let replaced = self.writer.run(|| {
            // Snapshot first, then the ring, both under the writer lock: a
            // waker woken by this push already sees this value, and no other
            // writer can interleave (invariant 1). The snapshot lock is
            // released before the adapter's push wakes anyone (invariant 2).
            let replaced = self.latest.with(|latest| latest.replace(snapshot));
            self.inner.push(value);
            replaced
        });
        // (D) outside every lock.
        drop(replaced);
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
        // Copy the pointer under the lock; clone and drop outside it.
        let snapshot = self.latest.with(|latest| latest.clone())?;
        Some(T::clone(&snapshot))
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
    //! Invariants 1–4 against a fake inner buffer. The same cells run against
    //! every adapter's real ring in `test_support::assert_latest_slot_contract`.
    use super::*;
    use crate::DbError;
    use alloc::sync::Weak;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use core::task::{Context, Poll};

    type Hook<T> = Arc<dyn Fn(&LatestSlot<T>) + Send + Sync>;
    type WakeHook<T> = spin::Mutex<Option<(Weak<LatestSlot<T>>, Hook<T>)>>;
    #[cfg(feature = "std")]
    type WriteHook<T> = spin::Mutex<Option<Arc<dyn Fn(&T) + Send + Sync>>>;

    /// A ring stand-in: logs pushes in order, then runs a "wake" hook
    /// synchronously, inside `push`, as tokio's and Embassy's rings do.
    struct Ring<T: Clone + Send> {
        log: spin::Mutex<Vec<T>>,
        /// Yield before logging, to widen the window between the snapshot
        /// update and the ring write, so a missing writer lock shows up.
        #[cfg(feature = "std")]
        yield_before_write: bool,
        /// Runs before the write; lets a test hold one writer inside it.
        #[cfg(feature = "std")]
        before_write: WriteHook<T>,
        on_wake: WakeHook<T>,
    }

    impl<T: Clone + Send> Ring<T> {
        fn last(&self) -> Option<T> {
            self.log.lock().last().cloned()
        }
    }

    struct NoReader;
    impl<T: Clone + Send> BufferReader<T> for NoReader {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
            Poll::Pending
        }
        fn try_recv(&mut self) -> Result<T, DbError> {
            Err(DbError::BufferEmpty)
        }
    }

    impl<T: Clone + Send + Sync + 'static> DynBuffer<T> for Ring<T> {
        fn push(&self, value: T) {
            #[cfg(feature = "std")]
            if self.yield_before_write {
                extern crate std;
                std::thread::yield_now();
            }
            #[cfg(feature = "std")]
            {
                let hook = self.before_write.lock().clone();
                if let Some(hook) = hook {
                    hook(&value);
                }
            }
            self.log.lock().push(value);
            let hook = self.on_wake.lock().clone();
            if let Some((slot, hook)) = hook {
                if let Some(slot) = slot.upgrade() {
                    hook(&slot);
                }
            }
        }
        fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
            Box::new(NoReader)
        }
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    fn ring<T: Clone + Send + Sync + 'static>() -> (Arc<Ring<T>>, Arc<LatestSlot<T>>) {
        let ring = Arc::new(Ring {
            log: spin::Mutex::new(Vec::new()),
            #[cfg(feature = "std")]
            yield_before_write: false,
            #[cfg(feature = "std")]
            before_write: spin::Mutex::new(None),
            on_wake: spin::Mutex::new(None),
        });
        let slot = Arc::new(LatestSlot::new(ring.clone() as Arc<dyn DynBuffer<T>>));
        (ring, slot)
    }

    fn on_wake<T: Clone + Send>(ring: &Ring<T>, slot: &Arc<LatestSlot<T>>, hook: Hook<T>) {
        *ring.on_wake.lock() = Some((Arc::downgrade(slot), hook));
    }

    /// Runs `f`, failing with "deadlock" instead of hanging if it does not
    /// return within 5 s (std); runs it directly elsewhere.
    fn no_deadlock(f: impl FnOnce() + Send + 'static) {
        #[cfg(feature = "std")]
        {
            extern crate std;
            let (done, rx) = std::sync::mpsc::channel();
            let t = std::thread::spawn(move || {
                f();
                let _ = done.send(());
            });
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(()) => t.join().unwrap(),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // `f` panicked: re-raise its panic.
                    if let Err(e) = t.join() {
                        std::panic::resume_unwind(e);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("deadlock: no return within 5 s")
                }
            }
        }
        #[cfg(not(feature = "std"))]
        f();
    }

    #[test]
    fn peek_is_none_before_the_first_push() {
        let (_, slot) = ring::<u32>();
        assert_eq!(slot.peek(), None);
    }

    #[test]
    fn peek_returns_the_last_push_and_the_ring_gets_every_push() {
        let (ring, slot) = ring::<u32>();
        slot.push(1);
        slot.push(2);
        assert_eq!(slot.peek(), Some(2));
        assert_eq!(slot.peek(), Some(2), "peek does not consume");
        assert_eq!(*ring.log.lock(), [1, 2]);
    }

    #[test]
    fn as_any_downcasts_to_the_inner_buffer() {
        let (_, slot) = ring::<u32>();
        assert!(slot.as_any().downcast_ref::<Ring<u32>>().is_some());
    }

    /// Invariants 1 + 2 (W × peek): the waker sees the value being pushed.
    #[test]
    fn a_waker_can_peek_the_ring_it_was_woken_by() {
        no_deadlock(|| {
            let (ring, slot) = ring::<u32>();
            let seen = Arc::new(spin::Mutex::new(Vec::new()));
            let log = seen.clone();
            on_wake(&ring, &slot, Arc::new(move |s| log.lock().push(s.peek())));
            slot.push(1);
            slot.push(2);
            assert_eq!(*seen.lock(), [Some(1), Some(2)]);
        });
    }

    /// Invariant 3 (W × push same ring): ends on the ring's last value.
    #[test]
    fn a_waker_can_push_to_the_ring_it_was_woken_by() {
        no_deadlock(|| {
            let (ring, slot) = ring::<u32>();
            let once = Arc::new(AtomicBool::new(true));
            on_wake(
                &ring,
                &slot,
                Arc::new(move |s| {
                    if once.swap(false, SeqCst) {
                        s.push(20);
                    }
                }),
            );
            slot.push(10);
            assert_eq!(*ring.log.lock(), [10, 20]);
            assert_eq!(slot.peek(), ring.last());
        });
    }

    /// W × push other ring: a waker of ring A pushes to ring B.
    #[test]
    fn a_waker_can_push_to_another_ring() {
        no_deadlock(|| {
            let (ring_a, slot_a) = ring::<u32>();
            let (ring_b, slot_b) = ring::<u32>();
            let target = slot_b.clone();
            on_wake(&ring_a, &slot_a, Arc::new(move |_| target.push(7)));
            slot_a.push(1);
            assert_eq!(slot_b.peek(), Some(7));
            assert_eq!(slot_b.peek(), ring_b.last());
            assert_eq!(slot_a.peek(), ring_a.last());
        });
    }

    /// A value whose `Clone` and `Drop` read the ring it lives in.
    struct Probe {
        n: u32,
        slot: Arc<spin::Mutex<Weak<LatestSlot<Probe>>>>,
        calls: Arc<AtomicUsize>,
        busy: Arc<AtomicBool>,
    }

    impl Probe {
        fn fire(&self) {
            // Guard: the peek below clones and drops a Probe itself.
            if self.busy.swap(true, SeqCst) {
                return;
            }
            let slot = self.slot.lock().upgrade();
            if let Some(slot) = slot {
                let _ = slot.peek();
            }
            self.calls.fetch_add(1, SeqCst);
            self.busy.store(false, SeqCst);
        }
    }

    impl Clone for Probe {
        fn clone(&self) -> Self {
            self.fire();
            Self {
                n: self.n,
                slot: self.slot.clone(),
                calls: self.calls.clone(),
                busy: self.busy.clone(),
            }
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.fire();
        }
    }

    /// Invariant 2 (C × peek, D × peek): `T::clone` and `T::drop` run
    /// outside the snapshot lock, so they can read the same ring.
    #[test]
    fn clone_and_drop_can_peek_the_same_ring() {
        no_deadlock(|| {
            let (_ring, slot) = ring::<Probe>();
            let cell = Arc::new(spin::Mutex::new(Arc::downgrade(&slot)));
            let calls = Arc::new(AtomicUsize::new(0));
            let busy = Arc::new(AtomicBool::new(false));
            let probe = |n| Probe {
                n,
                slot: cell.clone(),
                calls: calls.clone(),
                busy: busy.clone(),
            };
            slot.push(probe(1)); // clone for the snapshot
            slot.push(probe(2)); // clone, and drop of the replaced snapshot
            assert_eq!(slot.peek().map(|p| p.n), Some(2)); // clone + drop
            assert!(calls.load(SeqCst) >= 4, "clone and drop each read the ring");
        });
    }

    /// Invariant 1 under real concurrency: after every push has returned,
    /// `peek` is the value the ring received last.
    #[cfg(feature = "std")]
    #[test]
    fn concurrent_writers_end_on_the_rings_last_value() {
        extern crate std;
        for _ in 0..20 {
            let ring = Arc::new(Ring {
                log: spin::Mutex::new(Vec::new()),
                yield_before_write: true,
                before_write: spin::Mutex::new(None),
                on_wake: spin::Mutex::new(None),
            });
            let slot = Arc::new(LatestSlot::new(ring.clone() as Arc<dyn DynBuffer<u32>>));
            let threads: Vec<_> = (0..8u32)
                .map(|t| {
                    let slot = slot.clone();
                    std::thread::spawn(move || {
                        for i in 0..500 {
                            slot.push(t * 1000 + i);
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            assert_eq!(ring.log.lock().len(), 4000);
            assert_eq!(slot.peek(), ring.last());
        }
    }

    /// Invariant 1, deterministically: writer A is held inside the ring write
    /// until writer B's push returns (or 200 ms pass). Without a writer lock
    /// B overtakes A, so the ring ends on A while the snapshot says B. With
    /// it, B waits for A and the two agree.
    #[cfg(feature = "std")]
    #[test]
    fn a_second_writer_cannot_overtake_one_inside_the_ring_write() {
        extern crate std;
        use std::sync::mpsc;
        use std::time::Duration;

        let (ring, slot) = ring::<u32>();
        let (b_done, b_done_rx) = mpsc::channel::<()>();
        let (a_inside, a_inside_rx) = mpsc::channel::<()>();
        let b_done_rx = std::sync::Mutex::new(b_done_rx);
        let a_inside = std::sync::Mutex::new(a_inside);
        *ring.before_write.lock() = Some(Arc::new(move |v: &u32| {
            if *v == 1 {
                a_inside.lock().unwrap().send(()).unwrap();
                let _ = b_done_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(200));
            }
        }));

        let a = {
            let slot = slot.clone();
            std::thread::spawn(move || slot.push(1))
        };
        a_inside_rx.recv().unwrap();
        let b = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                slot.push(2);
                let _ = b_done.send(());
            })
        };
        a.join().unwrap();
        b.join().unwrap();
        assert_eq!(slot.peek(), ring.last());
    }

    /// Invariant 4: with `critical-section`, push and peek nest inside a
    /// critical section that is already held (an interrupt handler, or an
    /// adapter taking its own).
    #[cfg(all(not(feature = "std"), feature = "critical-section"))]
    #[test]
    fn push_and_peek_nest_inside_a_critical_section() {
        let (ring, slot) = ring::<u32>();
        critical_section::with(|_| {
            slot.push(7);
            assert_eq!(slot.peek(), Some(7));
        });
        assert_eq!(slot.peek(), ring.last());
    }
}
