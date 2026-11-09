//! Shared behavioral contract for [`Buffer`] implementations.
//!
//! Panicking is how a harness reports.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test harness reports by panicking"
)]
//!
//! Each adapter crate calls these from a test running under its own executor
//! (`#[tokio::test]`, `block_on`, or a host `#[test]` + `block_on`), so the one
//! contract is exercised against every runtime buffer implementation — the
//! buffer analogue of [`crate::executor::test_support`]. It moves cross-runtime
//! buffer parity from per-adapter discipline to a single suite every adapter
//! must pass.
//!
//! # Executor-agnostic by construction
//!
//! The suite is `async` but never `.await`s a `recv()` that cannot make
//! progress: it only awaits a receive when a value is already available, and
//! uses [`Reader::try_recv`] for every empty/pending assertion. So it resolves
//! under any executor — including a bare `futures::executor::block_on` on a
//! single-threaded host — without hanging.
//!
//! Buffers hold `Stamped<i32>` values, as record buffers hold `Stamped<T>`.
//! Each sample's stamp equals its value, so comparing whole samples also checks
//! that a buffer carries write metadata through unchanged.
//!
//! Each `assert_*` takes an `mk` closure that constructs a fresh buffer of the
//! kind under test. The closure (rather than a [`BufferCfg`]) lets each adapter
//! supply its own concrete type and const-generic sizing — e.g. the embassy
//! adapter's `EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>` — while the suite
//! stays generic over the [`Buffer`] + [`DynBuffer`] surface.
//!
//! [`BufferCfg`]: crate::buffer::BufferCfg

use alloc::boxed::Box;

use super::{Buffer, DynBuffer, Reader};
use crate::{DbError, Stamp, Stamped, WriteMetadata};

/// A sample whose stamp equals its value.
pub fn sample(v: i32) -> Stamped<i32> {
    Stamped::new(v, WriteMetadata::new(Stamp::from_nanos(v as u64)))
}

/// Wrap a freshly-subscribed reader in the ergonomic [`Reader`] handle.
fn reader<B: Buffer<Stamped<i32>>>(buffer: &B) -> Reader<i32> {
    Reader::new(Box::new(buffer.subscribe()))
}

/// Asserts the behavioral contract every `SingleLatest` buffer must hold.
///
/// Covers the fresh-subscriber rule (the divergence this suite exists to
/// prevent from recurring), plus overwrite collapsing and non-destructive
/// [`peek`](DynBuffer::peek).
pub async fn assert_single_latest_contract<B, F>(mk: F)
where
    B: Buffer<Stamped<i32>> + DynBuffer<Stamped<i32>>,
    F: Fn() -> B,
{
    // 1. Fresh subscriber observes a value published *before* it subscribed,
    //    exactly once — the core cross-runtime guarantee.
    let buffer = mk();
    Buffer::push(&buffer, sample(42));
    let mut r = reader(&buffer);
    assert_eq!(
        r.recv().await.unwrap(),
        sample(42),
        "fresh SingleLatest subscriber must observe the current value once"
    );
    assert!(
        matches!(r.try_recv(), Err(DbError::BufferEmpty)),
        "no new push after the first delivery must read empty, not redeliver"
    );

    // 2. Fresh subscriber with nothing published yet waits — it must not error.
    let buffer = mk();
    let mut r = reader(&buffer);
    assert!(
        matches!(r.try_recv(), Err(DbError::BufferEmpty)),
        "nothing produced yet must read empty, not a spurious value or closure"
    );
    Buffer::push(&buffer, sample(7));
    assert_eq!(r.recv().await.unwrap(), sample(7));

    // 3. Overwrite: intermediate values collapse to the latest.
    let buffer = mk();
    let mut r = reader(&buffer);
    Buffer::push(&buffer, sample(1));
    Buffer::push(&buffer, sample(2));
    Buffer::push(&buffer, sample(3));
    assert_eq!(
        r.recv().await.unwrap(),
        sample(3),
        "SingleLatest keeps only the newest value"
    );

    // 4. peek() is non-destructive and reflects the latest value.
    let buffer = mk();
    assert_eq!(
        DynBuffer::peek(&buffer),
        None,
        "peek before any push must be None"
    );
    Buffer::push(&buffer, sample(10));
    Buffer::push(&buffer, sample(20));
    assert_eq!(DynBuffer::peek(&buffer), Some(sample(20)));
    assert_eq!(
        DynBuffer::peek(&buffer),
        Some(sample(20)),
        "peek must be non-destructive"
    );
}

/// Asserts the behavioral contract every `Mailbox` buffer must hold.
///
/// Covers single-slot overwrite, take-on-read draining, empty-buffer behavior,
/// and non-destructive [`peek`](DynBuffer::peek) of the pending slot.
pub async fn assert_mailbox_contract<B, F>(mk: F)
where
    B: Buffer<Stamped<i32>> + DynBuffer<Stamped<i32>>,
    F: Fn() -> B,
{
    // Overwrite: an unconsumed value is replaced by a newer push.
    let buffer = mk();
    let mut r = reader(&buffer);
    Buffer::push(&buffer, sample(1));
    Buffer::push(&buffer, sample(2));
    Buffer::push(&buffer, sample(3));
    assert_eq!(
        r.recv().await.unwrap(),
        sample(3),
        "Mailbox keeps only the latest unconsumed value"
    );
    // Take-on-read empties the slot.
    assert!(
        matches!(r.try_recv(), Err(DbError::BufferEmpty)),
        "the slot must be drained once received"
    );

    // Empty buffer reads empty (not an error).
    let buffer = mk();
    let mut r = reader(&buffer);
    assert!(matches!(r.try_recv(), Err(DbError::BufferEmpty)));

    // peek() reflects the pending slot without draining it.
    let buffer = mk();
    assert_eq!(DynBuffer::peek(&buffer), None);
    Buffer::push(&buffer, sample(7));
    assert_eq!(DynBuffer::peek(&buffer), Some(sample(7)));
    assert_eq!(
        DynBuffer::peek(&buffer),
        Some(sample(7)),
        "peek must not drain the mailbox slot"
    );
}

/// Asserts the behavioral contract every `SpmcRing` buffer must hold.
///
/// Covers independent per-reader cursors, lag surfacing as
/// [`DbError::BufferLagged`], and that [`peek`](DynBuffer::peek) returns `None`
/// (a ring has no canonical latest).
///
/// `mk` must produce a ring small enough that the `LAG_PUSHES` below overflow
/// it — every adapter's own tests already use a small capacity (e.g. 4).
pub async fn assert_spmc_ring_contract<B, F>(mk: F)
where
    B: Buffer<Stamped<i32>> + DynBuffer<Stamped<i32>>,
    F: Fn() -> B,
{
    // Far more pushes than any test ring capacity, so a reader that never
    // drains is guaranteed to fall behind.
    const LAG_PUSHES: i32 = 64;

    // Independent cursors: two readers each see every value at their own pace.
    let buffer = mk();
    let mut a = reader(&buffer);
    let mut b = reader(&buffer);
    Buffer::push(&buffer, sample(1));
    Buffer::push(&buffer, sample(2));
    assert_eq!(a.recv().await.unwrap(), sample(1));
    assert_eq!(b.recv().await.unwrap(), sample(1));
    assert_eq!(a.recv().await.unwrap(), sample(2));
    assert_eq!(b.recv().await.unwrap(), sample(2));
    assert!(
        matches!(a.try_recv(), Err(DbError::BufferEmpty)),
        "a caught-up reader reads empty"
    );

    // Lag surfaces as BufferLagged. The reader subscribes first (cursor at the
    // start), then a burst overflows the ring before it reads.
    let buffer = mk();
    let mut r = reader(&buffer);
    for i in 0..LAG_PUSHES {
        Buffer::push(&buffer, sample(i));
    }
    assert!(
        matches!(r.recv().await, Err(DbError::BufferLagged { .. })),
        "a slow reader on an overflowed ring must surface BufferLagged"
    );

    // A ring has no canonical latest value.
    assert_eq!(
        DynBuffer::peek(&buffer),
        None,
        "SpmcRing peek must be None (no canonical latest)"
    );
}

/// Whether an adapter's ring tolerates a push from inside one of its own
/// wakes, for [`assert_latest_slot_contract`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReentrantPush {
    /// The ring wakes readers after releasing its own lock (tokio), so a
    /// waker may push to the same ring.
    Supported,
    /// The ring wakes readers while holding its own lock (Embassy, wasm), so a
    /// re-entrant push fails inside the adapter. That cell is skipped.
    AdapterLimit,
}

/// Asserts the contract of the latest-value wrapper core puts around every
/// `SpmcRing` (`LatestSlot`, see `buffer/latest.rs`) against an adapter's
/// real ring, with its real, synchronous wakes.
///
/// `mk` must produce an `SpmcRing` whose capacity holds at least 4 values.
///
/// Cells covered (foreign code × what it does):
/// - a waker, run inside `push`, reads `peek` on the same ring: it sees the
///   value being pushed, without deadlock or panic;
/// - a waker pushes to the same ring (when `reentrant` is
///   [`ReentrantPush::Supported`]): `peek` ends on the ring's last value;
/// - the reader receives every value in ring order, and `peek` is
///   non-destructive.
pub async fn assert_latest_slot_contract<B, F>(mk: F, reentrant: ReentrantPush)
where
    B: Buffer<Stamped<i32>> + DynBuffer<Stamped<i32>> + 'static,
    F: Fn() -> B,
{
    use alloc::sync::Arc;
    use alloc::task::Wake;
    use alloc::vec::Vec;
    use core::task::{Context, Poll, Waker};

    use super::LatestSlot;

    type Slot = LatestSlot<Stamped<i32>>;

    /// On wake: read the slot, and optionally push `push_on_wake` once.
    struct Probe {
        slot: Arc<Slot>,
        seen: spin::Mutex<Vec<Option<Stamped<i32>>>>,
        push_on_wake: spin::Mutex<Option<Stamped<i32>>>,
    }
    impl Wake for Probe {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            let seen = self.slot.peek();
            self.seen.lock().push(seen);
            let push = self.push_on_wake.lock().take();
            if let Some(v) = push {
                self.slot.push(v);
            }
        }
    }

    let new_slot = || Arc::new(LatestSlot::new(Arc::new(mk()) as Arc<dyn DynBuffer<_>>));

    // ── A waker reads the same ring during the push that woke it ──────────
    let slot = new_slot();
    let probe = Arc::new(Probe {
        slot: slot.clone(),
        seen: spin::Mutex::new(Vec::new()),
        push_on_wake: spin::Mutex::new(None),
    });
    let waker = Waker::from(probe.clone());
    let mut reader = slot.subscribe_boxed();
    assert!(
        reader
            .poll_recv(&mut Context::from_waker(&waker))
            .is_pending(),
        "an empty ring is pending"
    );
    slot.push(sample(1));
    assert_eq!(
        probe.seen.lock().first().cloned(),
        Some(Some(sample(1))),
        "the push woke the reader synchronously, and the waker's peek saw the \
         value being pushed"
    );
    assert!(
        matches!(
            reader.poll_recv(&mut Context::from_waker(&waker)),
            Poll::Ready(Ok(ref v)) if *v == sample(1)
        ),
        "the reader receives the value"
    );
    assert_eq!(slot.peek(), Some(sample(1)), "peek is non-destructive");
    assert_eq!(slot.peek(), Some(sample(1)));

    // ── A waker pushes to the same ring ──────────────────────────────────
    if reentrant == ReentrantPush::Supported {
        let slot = new_slot();
        let probe = Arc::new(Probe {
            slot: slot.clone(),
            seen: spin::Mutex::new(Vec::new()),
            push_on_wake: spin::Mutex::new(Some(sample(3))),
        });
        let waker = Waker::from(probe.clone());
        let mut reader = slot.subscribe_boxed();
        assert!(reader
            .poll_recv(&mut Context::from_waker(&waker))
            .is_pending());
        // 2 enters the ring, wakes the probe, which pushes 3 from inside.
        slot.push(sample(2));
        let mut received = Vec::new();
        while let Poll::Ready(Ok(v)) = reader.poll_recv(&mut Context::from_waker(&waker)) {
            received.push(v);
        }
        assert_eq!(received, [sample(2), sample(3)], "ring order");
        assert_eq!(
            slot.peek(),
            received.last().cloned(),
            "peek ends on the value the ring received last"
        );
    }
}
