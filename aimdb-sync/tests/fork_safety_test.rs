//! What a `fork()`ed child is told about handles it inherited.
//!
//! Only what a unit test cannot prove. Every *refusal* path is now checked in
//! `runtime.rs` against a `Runtime` stamped with a stale generation — no
//! thread, no fork, no sleep — because the stamp became ordinary data when the
//! runtime became a value. What survives here needs a real
//! child process: that the `pthread_atfork` handler is genuinely installed and
//! fires, and that a destructor in that child does not join a thread this
//! process never had.
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
        // The same refusal, asked rather than provoked: `check()` has to agree
        // with the two publishes above.
        let check_agrees = matches!(inherited.check(), Err(SyncError::ForkedChild));
        // Leak rather than free. The child is about to `_exit`, which reclaims
        // everything anyway, and `free` is the unsafe act here: it takes the
        // allocator lock, which a thread that did not survive the fork may have
        // been holding. Measured over 60 runs: freeing here hung 11-17 times,
        // leaking hung once. What this test asserts is the refusal above, not
        // the destructor, so there is nothing to lose by not running it.
        std::mem::forget(inherited);

        refused && refused_try && check_agrees
    });
    assert_eq!(code, 0, "the child's publishes should have been refused");

    // The parent is unaffected: its runtime thread is still its own.
    producer
        .set(Reading { value: 3 })
        .expect("parent still publishes");
    handle.detach().expect("detach");
}

/// Dropping an inherited handle must not join a thread this process does not
/// have. That join panics inside `std` with "threads should not terminate
/// unexpectedly" — from a destructor, which across an FFI boundary means a Rust
/// backtrace on stderr during teardown.
#[test]
fn dropping_an_inherited_handle_does_not_panic() {
    let to_detach = attach();
    let to_drop = attach();

    // Made before the fork, so the child inherits a consumer that was
    // legitimate when created — and allocated in the parent, where that is safe.
    let mut inherited = to_drop
        .consumer::<Reading>("sensor.reading")
        .expect("consumer");

    let code = in_forked_child(move || {
        // A binding asks this from a signal handler, before acting.
        let reports_closed = to_detach.is_closed() && to_drop.is_closed();

        // `detach` reports the situation rather than joining.
        let refused = matches!(to_detach.detach(), Err(SyncError::ForkedChild));

        // A handle in a child hands out nothing *usable*. `consumer` refuses
        // outright, because subscribing goes through the fork-checked `db()`.
        // `producer` succeeds — it touches nothing, exactly as it does for an
        // unregistered key — and the refusal lands on first use instead. Both
        // are safe; what matters is that neither silently accepts a value.
        let producer_defers = match to_drop.producer::<Reading>("sensor.reading") {
            Ok(p) => matches!(p.set(Reading { value: 9 }), Err(SyncError::ForkedChild)),
            Err(_) => false,
        };
        let no_consumer = matches!(
            to_drop.consumer::<Reading>("sensor.reading"),
            Err(SyncError::ForkedChild)
        );

        // This one is never detached: it is dropped when the closure returns,
        // which is the destructor path. It must return quietly rather than
        // join. A panic here would unwind out of the child instead of exiting
        // normally, and the parent's `WIFEXITED` assertion would catch it.
        drop(to_drop);

        // Releasing the handle must not release the *refusal*: the runtime
        // thread was never here either way. Answering on the runtime's liveness
        // instead would call the buffer merely empty — and park a blocking read
        // forever — because the drop above is what a child is supposed to do.
        let still_refused = matches!(inherited.try_get(), Err(SyncError::ForkedChild));
        // Leak rather than free, per the module note in `fork_child`.
        std::mem::forget(inherited);

        reports_closed && refused && producer_defers && no_consumer && still_refused
    });
    assert_eq!(code, 0, "detach in a child should be refused, not fatal");
}
