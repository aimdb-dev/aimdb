//! What a `fork()`ed child is told about handles it inherited.
//!
//! `fork` copies the address space but not the threads, so the child holds a
//! handle whose runtime thread does not exist in this process. The failure this
//! guards is not a crash but a silence: before the generation check, the child's
//! `set()` returned `Ok` and the value went into a buffer nobody drains.
#![cfg(all(unix, feature = "std"))]
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::{AimDbBuilderSyncExt, SyncError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

mod fork_child;
use fork_child::in_forked_child;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    value: u32,
}

fn attach() -> aimdb_sync::AimDbHandle {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Reading>("sensor.reading", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .tap(|_ctx, _consumer| async move {});
    });
    builder.attach().expect("attach")
}

/// The whole point: the child is refused, not quietly accepted.
#[test]
fn a_forked_child_is_refused_rather_than_silently_dropped() {
    let handle = attach();
    let producer = handle
        .producer::<Reading>("sensor.reading")
        .expect("producer");
    let inherited = producer.clone();

    let code = in_forked_child(move || {
        // An inherited producer must refuse. Before the fork generation, this
        // returned Ok and the reading went nowhere.
        let refused = matches!(
            inherited.set(Reading { value: 1 }),
            Err(SyncError::ForkedChild)
        );
        let refused_try = matches!(
            inherited.try_set(Reading { value: 2 }),
            Err(SyncError::ForkedChild)
        );
        // Leak rather than free. The child is about to `_exit`, which reclaims
        // everything anyway, and `free` is the unsafe act here: it takes the
        // allocator lock, which a thread that did not survive the fork may have
        // been holding. Measured over 60 runs: freeing here hung 11-17 times,
        // leaking hung once. What this test asserts is the refusal above, not
        // the destructor, so there is nothing to lose by not running it.
        std::mem::forget(inherited);

        refused && refused_try
    });
    assert_eq!(code, 0, "the child's publishes should have been refused");

    // The parent is unaffected: its runtime thread is still its own.
    producer
        .set(Reading { value: 3 })
        .expect("parent still publishes");
    handle.detach().expect("detach");
}

/// An inherited *consumer* must refuse too, and must drop quietly.
///
/// Its reader would otherwise block forever: the buffer came across with the
/// address space, but the runtime thread that fills it did not.
#[test]
fn a_forked_child_is_refused_by_an_inherited_consumer() {
    let handle = attach();
    let mut inherited = handle
        .consumer::<Reading>("sensor.reading")
        .expect("consumer");

    let code = in_forked_child(move || {
        let refused_try = matches!(inherited.try_get(), Err(SyncError::ForkedChild));
        // `get` blocks in the parent; in the child it must return, not park.
        let refused_get = matches!(inherited.get(), Err(SyncError::ForkedChild));
        let refused_latest = matches!(inherited.get_latest(), Err(SyncError::ForkedChild));
        let refused_timeout = matches!(
            inherited.get_with_timeout(Duration::from_millis(50)),
            Err(SyncError::ForkedChild)
        );
        let refused_latest_timeout = matches!(
            inherited.get_latest_with_timeout(Duration::from_millis(50)),
            Err(SyncError::ForkedChild)
        );

        // Leaked, not dropped — see the note in the producer test. This test
        // asserts the five refusals above; `dropping_an_inherited_handle_does_
        // not_panic` is where the destructor itself is under test.
        std::mem::forget(inherited);

        refused_try && refused_get && refused_latest && refused_timeout && refused_latest_timeout
    });
    assert_eq!(
        code, 0,
        "every read on an inherited consumer should be refused"
    );

    handle.detach().expect("detach");
}

/// The handle must stop handing out new producers and consumers too, or the
/// guard is trivially bypassed by making a fresh one in the child.
#[test]
fn a_forked_child_cannot_make_new_producers_or_consumers() {
    let handle = attach();

    let code = in_forked_child(move || {
        let no_producer = matches!(
            handle.producer::<Reading>("sensor.reading"),
            Err(SyncError::ForkedChild)
        );
        let no_consumer = matches!(
            handle.consumer::<Reading>("sensor.reading"),
            Err(SyncError::ForkedChild)
        );
        std::mem::forget(handle);

        no_producer && no_consumer
    });
    assert_eq!(
        code, 0,
        "the child should get no new producers or consumers"
    );
}

/// Dropping an inherited handle must not join a thread this process does not
/// have. That join panics inside `std` with "threads should not terminate
/// unexpectedly" — from a destructor, which across an FFI boundary means a Rust
/// backtrace on stderr during teardown.
#[test]
fn dropping_an_inherited_handle_does_not_panic() {
    let to_detach = attach();
    let to_drop = attach();

    let code = in_forked_child(move || {
        // `detach` reports the situation rather than joining.
        let refused = matches!(to_detach.detach(), Err(SyncError::ForkedChild));

        // This one is never detached: it is dropped when the closure returns,
        // which is the destructor path. It must return quietly rather than
        // join. A panic here would unwind out of the child instead of exiting
        // normally, and the parent's `WIFEXITED` assertion would catch it.
        drop(to_drop);

        refused
    });
    assert_eq!(code, 0, "detach in a child should be refused, not fatal");
}
