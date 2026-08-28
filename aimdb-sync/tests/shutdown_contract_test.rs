//! The four properties a foreign-language binding needs from shutdown.
//!
//! `shutdown` takes `&self`, is idempotent, is safe to call while producers
//! publish, and `is_closed` never waits on the lock it holds. None of that is
//! visible in a signature — Rust has no interpreter lock and no `free`
//! function — so each one is pinned here rather than remembered.
//!
//! They are not academic. A `#[pymethods]` method and a C ABI's free function
//! both fail to receive `self` by value, so a binding that could only shut down
//! by value had to reach for `&mut self`, and that collided at run time with a
//! publish already in flight: 200/200 closes refused while one thread published
//! in a loop. The station crate above this one solved it once; these tests are
//! what stop the next binding from having to.
#![cfg(feature = "std")]
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::{AimDbBuilderSyncExt, AimDbHandle, SyncError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    value: u32,
}

fn attach() -> AimDbHandle {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Reading>("sensor.reading", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
            .tap(|_ctx, _consumer| async move {});
    });
    builder.attach().expect("attach")
}

/// The signature property: shutdown through a shared reference, from a thread
/// that does not own the handle.
///
/// This is the shape a signal handler has, and the shape every FFI door has.
#[test]
fn shutdown_takes_a_shared_reference() {
    let handle = Arc::new(attach());
    assert!(!handle.is_closed(), "a fresh handle is open");

    let from_elsewhere = Arc::clone(&handle);
    thread::spawn(move || from_elsewhere.shutdown())
        .join()
        .expect("the shutting-down thread should not panic")
        .expect("shutdown");

    assert!(
        handle.is_closed(),
        "closed after another thread shut it down"
    );
}

/// Idempotent, including when two threads race for it.
///
/// Both must succeed: a second caller finding the thread already taken is the
/// ordinary outcome of a `with` block ending inside a signal handler, not an
/// error to report.
#[test]
fn shutdown_is_idempotent() {
    let handle = attach();
    handle.shutdown().expect("first shutdown");
    handle.shutdown().expect("second shutdown is a no-op");
    handle.shutdown().expect("and so is the third");

    let racing = Arc::new(attach());
    let racers: Vec<_> = (0..4)
        .map(|_| {
            let h = Arc::clone(&racing);
            thread::spawn(move || h.shutdown())
        })
        .collect();
    for racer in racers {
        racer
            .join()
            .expect("no racer panics")
            .expect("no racer manufactures an error for losing");
    }
    assert!(racing.is_closed());
}

/// Shutdown while four threads publish: it must not queue behind a publish, and
/// no publish may fail for any reason but the shutdown itself.
///
/// A producer reaches the database through its own `Weak`, so it never takes
/// the lock shutdown holds. If that ever changes, this test times out instead
/// of the next FFI door discovering it against a live broker.
#[test]
fn shutdown_completes_while_producers_publish() {
    let handle = Arc::new(attach());
    let stop = Arc::new(AtomicBool::new(false));
    let unexpected = Arc::new(AtomicU64::new(0));
    let published = Arc::new(AtomicU64::new(0));

    let publishers: Vec<_> = (0..4)
        .map(|_| {
            let producer = handle
                .producer::<Reading>("sensor.reading")
                .expect("producer");
            let stop = Arc::clone(&stop);
            let unexpected = Arc::clone(&unexpected);
            let published = Arc::clone(&published);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match producer.set(Reading { value: 1 }) {
                        Ok(()) => {
                            published.fetch_add(1, Ordering::Relaxed);
                        }
                        // The one failure a shutdown is allowed to cause.
                        Err(SyncError::RuntimeShutdown) => break,
                        Err(_) => {
                            unexpected.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            })
        })
        .collect();

    // Let the publishers get into their loop, so the shutdown lands mid-flight
    // rather than before the first `set`.
    while published.load(Ordering::Relaxed) < 16 {
        thread::yield_now();
    }

    let began = Instant::now();
    handle.shutdown().expect("shutdown during publishes");
    let took = began.elapsed();

    stop.store(true, Ordering::Relaxed);
    for publisher in publishers {
        publisher.join().expect("no publisher panics");
    }

    assert!(
        took < Duration::from_secs(5),
        "shutdown queued behind a publish: took {:?}",
        took
    );
    assert_eq!(
        unexpected.load(Ordering::Relaxed),
        0,
        "a publish failed for something other than the shutdown"
    );
    assert!(handle.is_closed());
}

/// `is_closed` from another thread while a shutdown is in flight.
///
/// The reader must keep answering throughout — it reads an atomic and a relaxed
/// fork check, never the lock shutdown holds. A getter that waited on that lock
/// deadlocks a Python door under the GIL and a C++ door under whatever its log
/// callback holds, and neither is a compile error.
#[test]
fn is_closed_never_waits_for_a_shutdown_in_flight() {
    let handle = Arc::new(attach());
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let saw_closed = Arc::new(AtomicBool::new(false));

    let reader = {
        let handle = Arc::clone(&handle);
        let stop = Arc::clone(&stop);
        let ticks = Arc::clone(&ticks);
        let saw_closed = Arc::clone(&saw_closed);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if handle.is_closed() {
                    saw_closed.store(true, Ordering::Relaxed);
                }
                ticks.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    while ticks.load(Ordering::Relaxed) < 16 {
        thread::yield_now();
    }
    let before = ticks.load(Ordering::Relaxed);

    handle.shutdown().expect("shutdown");

    // The reader ticked *across* the shutdown rather than parking in it.
    assert!(
        ticks.load(Ordering::Relaxed) > before,
        "is_closed stopped answering while a shutdown was in flight"
    );

    stop.store(true, Ordering::Relaxed);
    reader.join().expect("the reader thread should not panic");

    assert!(
        saw_closed.load(Ordering::Relaxed),
        "the reader never observed the shutdown"
    );
    assert!(handle.is_closed());
}

/// A bounded shutdown reports what it left behind — and a later one does not
/// wait again, because the thread was released rather than kept.
#[test]
fn shutdown_timeout_releases_the_thread() {
    let handle = attach();
    handle
        .shutdown_timeout(Duration::from_secs(5))
        .expect("a healthy runtime thread stops well inside five seconds");
    assert!(handle.is_closed());

    let began = Instant::now();
    handle
        .shutdown()
        .expect("the second call has nothing to do");
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "a shutdown after a timed one waited again"
    );
}

/// The by-value doors are the same call, so a caller who has the handle by
/// value is not on a different contract.
#[test]
fn detach_is_shutdown_by_value() {
    attach().detach().expect("detach");
    attach()
        .detach_timeout(Duration::from_secs(5))
        .expect("detach_timeout");
}

/// A producer outliving the shutdown fails with `RuntimeShutdown`, not with
/// silence — the property `is_closed` is a cheap proxy for.
#[test]
fn a_publish_after_shutdown_is_refused() {
    let handle = attach();
    let producer = handle
        .producer::<Reading>("sensor.reading")
        .expect("producer");
    producer
        .set(Reading { value: 1 })
        .expect("publishes while open");

    handle.shutdown().expect("shutdown");

    assert!(matches!(
        producer.set(Reading { value: 2 }),
        Err(SyncError::RuntimeShutdown)
    ));
}

/// A consumer parked in `get()` must be woken by a shutdown on another thread.
///
/// This is what makes `shutdown(&self)` a shutdown rather than a stop button.
/// `aimdb-core` has no explicit close, so the wake comes from dropping the last
/// `Arc<AimDb>` and closing the buffers with it — which is why the handle owns
/// that reference and releases it here, rather than holding it until the
/// caller drops the handle. Before, the wake arrived only when the whole handle
/// went; an FFI door that shuts down and then joins its reader threads would
/// have hung.
#[test]
fn a_parked_consumer_wakes_when_another_thread_shuts_down() {
    let handle = Arc::new(attach());
    let mut consumer = handle
        .consumer::<Reading>("sensor.reading")
        .expect("consumer");

    let parked = thread::spawn(move || consumer.get());

    // Nothing is ever published, so the reader is parked rather than racing.
    thread::sleep(Duration::from_millis(50));

    let began = Instant::now();
    handle.shutdown().expect("shutdown");

    let woken = parked.join().expect("the parked thread should not panic");
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the parked consumer was not woken by the shutdown"
    );
    assert!(
        matches!(woken, Err(SyncError::RuntimeShutdown)),
        "a woken consumer reports the shutdown, got {:?}",
        woken
    );
}
