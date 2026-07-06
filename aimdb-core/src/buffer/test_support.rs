//! Shared behavioral contract for [`Buffer`] implementations.
//!
//! Each adapter crate calls these from a test running under its own executor
//! (`#[tokio::test]`, `block_on`, or a host `#[test]` + `block_on`), so the one
//! contract is exercised against every runtime buffer implementation — the
//! buffer analogue of [`crate::executor::test_support`]. This is the structural
//! fix behind design 040: cross-runtime buffer parity moves from per-adapter
//! discipline to a single suite every adapter must pass.
//!
//! # Executor-agnostic by construction
//!
//! The suite is `async` but never `.await`s a `recv()` that cannot make
//! progress: it only awaits a receive when a value is already available, and
//! uses [`Reader::try_recv`] for every empty/pending assertion. So it resolves
//! under any executor — including a bare `futures::executor::block_on` on a
//! single-threaded host — without hanging.
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
use crate::DbError;

/// Wrap a freshly-subscribed reader in the ergonomic [`Reader`] handle.
fn reader<B: Buffer<i32>>(buffer: &B) -> Reader<i32> {
    Reader::new(Box::new(buffer.subscribe()))
}

/// Asserts the behavioral contract every `SingleLatest` buffer must hold.
///
/// Covers the design-040 fresh-subscriber rule (the divergence this suite
/// exists to prevent from recurring), plus overwrite collapsing and non-
/// destructive [`peek`](DynBuffer::peek).
pub async fn assert_single_latest_contract<B, F>(mk: F)
where
    B: Buffer<i32> + DynBuffer<i32>,
    F: Fn() -> B,
{
    // 1. Fresh subscriber observes a value published *before* it subscribed,
    //    exactly once — the core cross-runtime guarantee.
    let buffer = mk();
    Buffer::push(&buffer, 42);
    let mut r = reader(&buffer);
    assert_eq!(
        r.recv().await.unwrap(),
        42,
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
    Buffer::push(&buffer, 7);
    assert_eq!(r.recv().await.unwrap(), 7);

    // 3. Overwrite: intermediate values collapse to the latest.
    let buffer = mk();
    let mut r = reader(&buffer);
    Buffer::push(&buffer, 1);
    Buffer::push(&buffer, 2);
    Buffer::push(&buffer, 3);
    assert_eq!(
        r.recv().await.unwrap(),
        3,
        "SingleLatest keeps only the newest value"
    );

    // 4. peek() is non-destructive and reflects the latest value.
    let buffer = mk();
    assert_eq!(
        DynBuffer::peek(&buffer),
        None,
        "peek before any push must be None"
    );
    Buffer::push(&buffer, 10);
    Buffer::push(&buffer, 20);
    assert_eq!(DynBuffer::peek(&buffer), Some(20));
    assert_eq!(
        DynBuffer::peek(&buffer),
        Some(20),
        "peek must be non-destructive"
    );
}

/// Asserts the behavioral contract every `Mailbox` buffer must hold.
///
/// Covers single-slot overwrite, take-on-read draining, empty-buffer behavior,
/// and non-destructive [`peek`](DynBuffer::peek) of the pending slot.
pub async fn assert_mailbox_contract<B, F>(mk: F)
where
    B: Buffer<i32> + DynBuffer<i32>,
    F: Fn() -> B,
{
    // Overwrite: an unconsumed value is replaced by a newer push.
    let buffer = mk();
    let mut r = reader(&buffer);
    Buffer::push(&buffer, 1);
    Buffer::push(&buffer, 2);
    Buffer::push(&buffer, 3);
    assert_eq!(
        r.recv().await.unwrap(),
        3,
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
    Buffer::push(&buffer, 7);
    assert_eq!(DynBuffer::peek(&buffer), Some(7));
    assert_eq!(
        DynBuffer::peek(&buffer),
        Some(7),
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
    B: Buffer<i32> + DynBuffer<i32>,
    F: Fn() -> B,
{
    // Far more pushes than any test ring capacity, so a reader that never
    // drains is guaranteed to fall behind.
    const LAG_PUSHES: i32 = 64;

    // Independent cursors: two readers each see every value at their own pace.
    let buffer = mk();
    let mut a = reader(&buffer);
    let mut b = reader(&buffer);
    Buffer::push(&buffer, 1);
    Buffer::push(&buffer, 2);
    assert_eq!(a.recv().await.unwrap(), 1);
    assert_eq!(b.recv().await.unwrap(), 1);
    assert_eq!(a.recv().await.unwrap(), 2);
    assert_eq!(b.recv().await.unwrap(), 2);
    assert!(
        matches!(a.try_recv(), Err(DbError::BufferEmpty)),
        "a caught-up reader reads empty"
    );

    // Lag surfaces as BufferLagged. The reader subscribes first (cursor at the
    // start), then a burst overflows the ring before it reads.
    let buffer = mk();
    let mut r = reader(&buffer);
    for i in 0..LAG_PUSHES {
        Buffer::push(&buffer, i);
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
