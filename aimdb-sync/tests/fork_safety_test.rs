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

/// Run `child` in a forked child and return its exit code.
///
/// The child ends with `_exit`, which skips both the test harness's cleanup and
/// every destructor — the child is not a test runner, and letting it unwind
/// would report a second set of results into the parent's stdout.
fn in_forked_child(child: impl FnOnce() -> bool) -> i32 {
    // SAFETY: `fork` itself is safe to call here. What the *child* may then do
    // is the real constraint, and it is not "anything": this parent is
    // multi-threaded — the runtime thread, plus whatever else the harness is
    // running — so the child starts out owning locks (the allocator's above
    // all) that were held by threads that no longer exist. Only
    // async-signal-safe work is strictly sound.
    //
    // The tests below stay as close to that as what they assert allows. The
    // refused accessors return a unit variant without allocating, which is the
    // bulk of it; the two that then drop inherited state do free memory, and
    // `a_child_can_attach_its_own_database_after_forking` allocates and spawns
    // a thread outright — proving a post-fork `attach` works is the whole point
    // of that one. So the residual risk of a rare hang is real rather than
    // argued away. Run this binary with `--test-threads=1` to keep the parent
    // as quiet as it can be at the moment of the fork.
    match unsafe { libc::fork() } {
        -1 => panic!("fork failed"),
        0 => {
            let ok = child();
            unsafe { libc::_exit(if ok { 0 } else { 1 }) }
        }
        pid => {
            let mut status: libc::c_int = 0;
            // SAFETY: `pid` is our child and `status` is a valid out-pointer.
            let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
            assert_eq!(waited, pid, "waitpid");
            assert!(
                libc::WIFEXITED(status),
                "child did not exit normally — a panic or signal, status {status}"
            );
            libc::WEXITSTATUS(status)
        }
    }
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

        // The destructor path, as for the handle above: must not block or
        // panic. The parent's `WIFEXITED` assertion catches a panic.
        drop(inherited);

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

/// A database attached *after* the fork is the child's own and must work.
/// This is why the guard is a generation counter rather than a poison flag.
#[test]
fn a_child_can_attach_its_own_database_after_forking() {
    let code = in_forked_child(|| {
        let handle = attach();
        let producer = match handle.producer::<Reading>("sensor.reading") {
            Ok(p) => p,
            Err(_) => return false,
        };
        let published = producer.set(Reading { value: 7 }).is_ok();
        let detached = handle.detach().is_ok();
        published && detached
    });
    assert_eq!(
        code, 0,
        "a post-fork attach belongs to the child and must work"
    );
}
