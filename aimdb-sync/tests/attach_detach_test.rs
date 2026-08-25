//! Attach and detach: the two paths that used to busy-wait.
//!
//! `AimDbSyncExt::attach` is covered here because it had no in-tree caller at
//! all — which is how an unbounded spin survived in it. Every other test in
//! this crate goes through `AimDbBuilderSyncExt::attach`.
#![cfg(feature = "std")]
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::{AimDbBuilderSyncExt, AimDbSyncExt, SyncError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    value: u32,
}

fn configured_builder() -> AimDbBuilder {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Reading>("sensor.reading", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .tap(|_ctx, _consumer| async move {});
    });
    builder
}

/// Run `f` on its own thread and fail if it has not finished in `limit`.
///
/// A watchdog rather than a plain call: the defect these tests cover is a
/// wait that never ends, and a test that reproduces it should fail rather
/// than hang the suite.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit)
        .unwrap_or_else(|_| panic!("did not finish within {:?}", limit))
}

/// `AimDbSyncExt::attach` — the entry point that carried the spin.
#[test]
fn attach_on_a_built_db_returns_a_usable_handle() {
    // `build()` is async, so it runs in a runtime that is dropped before
    // `attach()`: attach blocks, and must not be called from inside one.
    let db = {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (db, _runner) = rt.block_on(configured_builder().build()).expect("build");
        db
    };

    let handle = within(Duration::from_secs(10), move || db.attach()).expect("attach");
    handle
        .producer::<Reading>("sensor.reading")
        .expect("producer");
    handle.detach().expect("detach");
}

/// The reason a startup failed has to reach the caller, not only the log sink.
///
/// Before the channel carried a `Result`, this arrived as "Runtime thread
/// failed to build database" while the real cause went only to a log the
/// caller may never have installed.
#[test]
fn a_failed_build_reports_why() {
    // No `.runtime()`, so `build()` fails inside the runtime thread.
    let err = within(Duration::from_secs(10), || {
        AimDbBuilder::new().attach().err()
    })
    .expect("attach should fail");

    let SyncError::AttachFailed { message } = &err else {
        panic!("expected AttachFailed, got {:?}", err);
    };
    assert!(
        message.contains("runtime not set"),
        "the cause should survive the trip, got: {message}"
    );
}

/// A runtime thread that stops before reporting must end the wait, not extend
/// it — a closed channel is what turns the old spin into an error.
#[test]
fn attach_never_waits_forever() {
    let outcome = within(Duration::from_secs(10), || {
        AimDbBuilder::new().attach().map(|_| ())
    });
    assert!(outcome.is_err(), "a builder with no runtime cannot attach");
}

#[test]
fn detach_timeout_shuts_down_cleanly() {
    let handle = configured_builder().attach().expect("attach");
    let started = Instant::now();
    handle
        .detach_timeout(Duration::from_secs(5))
        .expect("detach within timeout");
    // Guards against a regression to a coarse poll; the wait now parks on a
    // channel rather than sleeping between checks.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown took {:?}",
        started.elapsed()
    );
}

#[test]
fn producer_and_consumer_work_through_the_attached_runtime() {
    let handle = configured_builder().attach().expect("attach");
    let producer = handle
        .producer::<Reading>("sensor.reading")
        .expect("producer");
    let mut consumer = handle
        .consumer::<Reading>("sensor.reading")
        .expect("consumer");

    producer.set(Reading { value: 42 }).expect("set");
    let got = consumer
        .get_with_timeout(Duration::from_secs(5))
        .expect("get");
    assert_eq!(got, Reading { value: 42 });

    handle.detach().expect("detach");
}
