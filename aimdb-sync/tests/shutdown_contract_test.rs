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
    let key_sensor_reading_1 = aimdb_core::Name::<Reading>::new("sensor.reading");
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure(key_sensor_reading_1, |reg| {
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
            let key_sensor_reading_2 = aimdb_core::Name::<Reading>::new("sensor.reading");
            let producer = handle.producer(key_sensor_reading_2).expect("producer");
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

/// Holds the runtime thread inside shutdown until the test releases it: the
/// tap future is dropped on that thread as the runtime winds down, before the
/// shutdown's `join` can return.
struct HoldShutdown {
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Drop for HoldShutdown {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        // Bounded, so a broken test fails instead of hanging the suite.
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10));
    }
}

/// `is_closed` keeps answering across a shutdown — a getter that waited on the
/// shutdown's lock deadlocks a Python door under the GIL.
#[test]
fn is_closed_never_waits_for_a_shutdown_in_flight() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let (started_tx, started_rx) = std::sync::mpsc::channel();

    let key = aimdb_core::Name::<Reading>::new("sensor.reading");
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure(key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
            .tap(move |_ctx, _consumer| {
                let hold = HoldShutdown {
                    entered: entered_tx.clone(),
                    release: Arc::clone(&release_rx),
                };
                let started = started_tx.clone();
                async move {
                    let _hold = hold;
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                }
            });
    });
    let handle = Arc::new(builder.attach().expect("attach"));
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the tap starts");
    assert!(!handle.is_closed(), "a fresh handle is open");

    let shutting_down = {
        let handle = Arc::clone(&handle);
        thread::spawn(move || handle.shutdown())
    };

    // The runtime thread is now parked inside the shutdown.
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown reaches the runtime thread");
    assert!(!shutting_down.is_finished(), "the shutdown is still in flight");

    // Asked from another thread, so a getter that waits on the shutdown's lock
    // shows up as a timeout rather than a hung test.
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    {
        let handle = Arc::clone(&handle);
        thread::spawn(move || {
            let _ = answer_tx.send(handle.is_closed());
        });
    }
    let answer = answer_rx.recv_timeout(Duration::from_secs(5));

    release_tx.send(()).expect("release the runtime thread");
    shutting_down
        .join()
        .expect("the shutting-down thread should not panic")
        .expect("shutdown");

    assert_eq!(
        answer,
        Ok(true),
        "is_closed stopped answering while a shutdown was in flight"
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
    let key_sensor_reading_3 = aimdb_core::Name::<Reading>::new("sensor.reading");
    let handle = attach();
    let producer = handle.producer(key_sensor_reading_3).expect("producer");
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
    let key_sensor_reading_4 = aimdb_core::Name::<Reading>::new("sensor.reading");
    let handle = Arc::new(attach());
    let mut consumer = handle.consumer(key_sensor_reading_4).expect("consumer");

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
