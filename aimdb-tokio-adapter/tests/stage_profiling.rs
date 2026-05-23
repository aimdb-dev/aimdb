//! Integration tests for automatic stage profiling (`profiling` feature).
//!
//! Registers a record with a periodic `.source()` and a slow `.tap()`, runs the
//! pipeline briefly, then inspects the per-stage `StageMetrics` collected on the
//! `TypedRecord` — verifying call counts, the `min <= avg <= max` invariant, and
//! that `.with_name(...)` is recorded.
//!
//! The whole module compiles to nothing without `--features profiling`.

#![cfg(feature = "profiling")]

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
                    let _ = producer.produce(Reading { value: n }).await;
                    n += 1;
                    ctx.time().sleep(ctx.time().millis(10)).await;
                }
            })
            .with_name("sensor_reader")
            .tap(|ctx, consumer| async move {
                let mut reader = consumer.subscribe().expect("subscribe");
                while let Ok(reading) = reader.recv().await {
                    // Simulate per-value processing work (wall-clock).
                    let _ = reading.value;
                    ctx.time().sleep(ctx.time().millis(5)).await;
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
        .get_typed_record_by_key::<Reading, TokioAdapter>(KEY)
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
    assert_eq!(s.avg_time_ns(), s.total_time_ns() / s.call_count());

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

#[tokio::test(flavor = "multi_thread")]
async fn with_name_is_a_no_op_friendly_builder() {
    // `.with_name()` must always be chainable even on a stage that is never run.
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    builder.configure::<Reading>("profiling::Unused", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(|_ctx, consumer| async move {
                let mut reader = consumer.subscribe().expect("subscribe");
                let _ = reader.recv().await;
            })
            .with_name("idle_tap");
    });
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());
    let rec = db
        .inner()
        .get_typed_record_by_key::<Reading, TokioAdapter>("profiling::Unused")
        .expect("typed record");
    assert_eq!(
        rec.profiling().tap(0).unwrap().name.as_deref(),
        Some("idle_tap")
    );
}
