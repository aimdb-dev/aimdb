//! Integration tests for automatic stage profiling (`profiling` feature).
//!
//! Registers a record with a periodic `.source()` and a slow `.tap()`, runs the
//! pipeline briefly, then inspects the per-stage `StageMetrics` collected on the
//! `TypedRecord` — verifying call counts, the `min <= avg <= max` invariant, and
//! that `.with_name(...)` is recorded.
//!
//! The whole module compiles to nothing without `--features profiling`.

#![cfg(feature = "observability")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
struct Reading {
    value: u64,
}

const KEY: &str = "profiling::Reading";

#[tokio::test(flavor = "multi_thread")]
async fn source_and_tap_stages_are_timed_and_named() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Reading>(KEY, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 32 })
            .source(|ctx, producer| async move {
                let mut n = 0u64;
                loop {
                    producer.produce(Reading { value: n });
                    n += 1;
                    ctx.time().sleep_millis(10).await;
                }
            })
            .with_name("sensor_reader")
            .tap(|ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(reading) = reader.recv().await {
                    // Simulate per-value processing work (wall-clock).
                    let _ = reading.value;
                    ctx.time().sleep_millis(5).await;
                }
            })
            .with_name("data_processor");
    });

    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    // Let the pipeline run for a bunch of iterations.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let rec = db
        .inner()
        .get_typed_record_by_key::<Reading>(KEY)
        .expect("typed record");
    let prof = rec.profiling();

    // Source stage.
    let source = prof.source(0).expect("source stage registered");
    assert_eq!(source.name.as_deref(), Some("sensor_reader"));
    let s = &source.metrics;
    assert!(
        s.call_count() >= 1,
        "expected at least one source interval recorded, got {}",
        s.call_count()
    );
    assert!(s.min_time_ns() <= s.avg_time_ns());
    assert!(s.avg_time_ns() <= s.max_time_ns());

    // Tap stage.
    let tap = prof.tap(0).expect("tap stage registered");
    assert_eq!(tap.name.as_deref(), Some("data_processor"));
    let t = &tap.metrics;
    assert!(
        t.call_count() >= 1,
        "expected at least one tap invocation recorded, got {}",
        t.call_count()
    );
    assert!(t.min_time_ns() <= t.avg_time_ns());
    assert!(t.avg_time_ns() <= t.max_time_ns());

    // No link stage was registered.
    assert!(prof.link(0).is_none());
    assert!(prof.links().is_empty());

    // Reset clears everything.
    prof.reset_all();
    assert_eq!(prof.source(0).unwrap().metrics.call_count(), 0);
    assert_eq!(prof.tap(0).unwrap().metrics.call_count(), 0);
}

#[derive(Debug, Clone)]
struct Doubled {
    value: u64,
}

const DOUBLED_KEY: &str = "profiling::Doubled";

/// Registers a single-input `.transform()` alongside a fast `.tap()` on the
/// derived record, and checks that the transform closure itself is timed
/// (not the producer cadence — see `run_single_transform`) and that its
/// average is correctly identified as the slower of the two stages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transform_stage_is_timed_and_is_the_bottleneck() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Reading>(KEY, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 32 })
            .source(|ctx, producer| async move {
                let mut n = 0u64;
                loop {
                    producer.produce(Reading { value: n });
                    n += 1;
                    ctx.time().sleep_millis(5).await;
                }
            })
            .with_name("fast_source");
    });

    builder.configure::<Doubled>(DOUBLED_KEY, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 32 })
            .transform::<Reading, _>(KEY, |b| {
                b.map(|input: &Reading| {
                    // Simulate expensive per-value work (wall-clock, synchronous —
                    // the transform closure has no `.await` point).
                    std::thread::sleep(Duration::from_millis(15));
                    Some(Doubled {
                        value: input.value * 2,
                    })
                })
            })
            .with_name("slow_doubler")
            .tap(|_ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(doubled) = reader.recv().await {
                    let _ = doubled.value;
                }
            })
            .with_name("quick_consumer");
    });

    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    tokio::time::sleep(Duration::from_millis(200)).await;

    let rec = db
        .inner()
        .get_typed_record_by_key::<Doubled>(DOUBLED_KEY)
        .expect("typed record");
    let prof = rec.profiling();

    // This record is derived via `.transform()`, not `.source()`.
    assert!(prof.source(0).is_none());

    let transform = prof.transform(0).expect("transform stage registered");
    assert_eq!(transform.name.as_deref(), Some("slow_doubler"));
    let tm = &transform.metrics;
    assert!(
        tm.call_count() >= 1,
        "expected at least one transform invocation recorded, got {}",
        tm.call_count()
    );
    assert!(tm.min_time_ns() <= tm.avg_time_ns());
    assert!(tm.avg_time_ns() <= tm.max_time_ns());

    let tap = prof.tap(0).expect("tap stage registered");
    assert_eq!(tap.name.as_deref(), Some("quick_consumer"));

    // The transform closure sleeps ~15ms per call; the tap is a no-op drain
    // loop. It must be reported as the slowest (bottleneck) stage on this
    // record.
    assert!(
        tm.avg_time_ns() > tap.metrics.avg_time_ns(),
        "expected transform (slow) avg {} ns > tap (fast) avg {} ns",
        tm.avg_time_ns(),
        tap.metrics.avg_time_ns()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn with_name_is_a_no_op_friendly_builder() {
    // `.with_name()` must always be chainable even on a stage that is never run.
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    builder.configure::<Reading>("profiling::Unused", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(|_ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                let _ = reader.recv().await;
            })
            .with_name("idle_tap");
    });
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());
    let rec = db
        .inner()
        .get_typed_record_by_key::<Reading>("profiling::Unused")
        .expect("typed record");
    assert_eq!(
        rec.profiling().tap(0).unwrap().name.as_deref(),
        Some("idle_tap")
    );
}
