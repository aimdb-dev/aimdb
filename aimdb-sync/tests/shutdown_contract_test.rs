//! What a foreign-language binding needs from shutdown, none of it visible in
//! a signature: `&self`, idempotent, safe during a publish, and an `is_closed`
//! that never waits on the shutdown's lock.
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

/// Shutdown from a thread that does not own the handle — a signal handler.
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

/// Idempotent, including under a race.
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

/// Shutdown must not queue behind a publish, and no publish may fail for
/// anything but the shutdown.
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
                        // The one failure a shutdown may cause.
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

    // So the shutdown lands mid-flight, not before the first `set`.
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

/// `is_closed` keeps answering across a shutdown — a getter that waited on the
/// shutdown's lock deadlocks a Python door under the GIL.
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

    // Ticked across the shutdown rather than parking in it.
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

/// A bounded shutdown releases the thread, so a later one does not wait.
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

/// The by-value doors are the same call, not a second contract.
#[test]
fn detach_is_shutdown_by_value() {
    attach().detach().expect("detach");
    attach()
        .detach_timeout(Duration::from_secs(5))
        .expect("detach_timeout");
}

/// A producer outliving the shutdown fails rather than publishing into
/// silence.
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

/// A consumer parked in `get()` is woken by a shutdown on another thread —
/// the wake is the last `Arc<AimDb>` dropping, so the handle must own it.
#[test]
fn a_parked_consumer_wakes_when_another_thread_shuts_down() {
    let handle = Arc::new(attach());
    let mut consumer = handle
        .consumer::<Reading>("sensor.reading")
        .expect("consumer");

    let parked = thread::spawn(move || consumer.get());

    // Nothing is published, so the reader parks rather than races.
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
