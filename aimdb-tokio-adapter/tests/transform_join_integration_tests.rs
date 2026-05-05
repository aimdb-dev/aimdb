//! Integration tests for `transform_join` (multi-input reactive transform).
//!
//! Scenario: two u32 inputs A and B, one output Sum = a + b emitted whenever
//! both have been seen at least once. Drives a fixed sequence and asserts that
//! the output values are produced in the expected order.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

// ---------------------------------------------------------------------------
// Record types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Copy)]
struct ValueA(u32);

#[derive(Clone, Debug, PartialEq, Copy)]
struct ValueB(u32);

#[derive(Clone, Debug, PartialEq, Copy)]
struct Sum(u32);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transform_join_produces_sum_on_both_inputs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    // SpmcRing inputs (vs SingleLatest) so that values produced before the join
    // transform's forwarders subscribe are still buffered and replayed — removes
    // a startup race where the test might otherwise need a hand-tuned barrier.
    builder.configure::<ValueA>("test::A", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure::<ValueB>("test::B", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure::<Sum>("test::Sum", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform_join(|b| {
                b.input::<ValueA>("test::A")
                    .input::<ValueB>("test::B")
                    .on_triggers(|mut rx, producer| async move {
                        let mut last_a: Option<u32> = None;
                        let mut last_b: Option<u32> = None;
                        while let Ok(trigger) = rx.recv().await {
                            match trigger.index() {
                                0 => last_a = trigger.as_input::<ValueA>().copied().map(|v| v.0),
                                1 => last_b = trigger.as_input::<ValueB>().copied().map(|v| v.0),
                                _ => {}
                            }
                            if let (Some(a), Some(b)) = (last_a, last_b) {
                                let _ = producer.produce(Sum(a + b)).await;
                            }
                        }
                    })
            });
    });

    let db = builder.build().await.unwrap();
    let mut sum_rx = db.subscribe::<Sum>("test::Sum").unwrap();

    // Yield to let the join transform task spawn its input forwarders and subscribe.
    tokio::task::yield_now().await;

    // A=1, B=10 → Sum=11 (first time both are present)
    db.produce::<ValueA>("test::A", ValueA(1)).await.unwrap();
    db.produce::<ValueB>("test::B", ValueB(10)).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.0, 11, "expected 1+10=11");

    // A=2 → Sum=12 (B stays 10)
    db.produce::<ValueA>("test::A", ValueA(2)).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.0, 12, "expected 2+10=12");

    // B=20 → Sum=22 (A stays 2)
    db.produce::<ValueB>("test::B", ValueB(20)).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.0, 22, "expected 2+20=22");
}

/// Stress test for the bounded(64) Tokio fan-in channel: pushes well over 64
/// events through a single-input join while the handler intentionally yields
/// between receives. If backpressure is wired correctly, this completes
/// without deadlock and every produced value is observed in order.
#[tokio::test]
async fn transform_join_bounded_fanin_backpressure_no_deadlock() {
    const N: u32 = 200;
    const SENTINEL: u32 = u32::MAX;
    let cap = (N + 16) as usize;

    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Input/output rings sized larger than the bounded(64) fan-in so the SpmcRing
    // itself isn't the limiter — we want the bounded channel to be the bottleneck.
    builder.configure::<ValueA>("stress::A", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: cap });
    });
    builder.configure::<Sum>("stress::Echo", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: cap })
            .transform_join(|b| {
                b.input::<ValueA>("stress::A")
                    .on_triggers(|mut rx, producer| async move {
                        while let Ok(trigger) = rx.recv().await {
                            if let Some(v) = trigger.as_input::<ValueA>().copied() {
                                // Yield between receives to keep the fan-in channel
                                // pressured well above its 64-slot capacity.
                                tokio::task::yield_now().await;
                                let _ = producer.produce(Sum(v.0)).await;
                            }
                        }
                    })
            });
    });

    let db = builder.build().await.unwrap();
    let mut echo_rx = db.subscribe::<Sum>("stress::Echo").unwrap();

    // Warm-up: keep producing a sentinel until its echo lands. SpmcRing buffers
    // are tokio broadcast channels, so subscribers (including the join input
    // forwarder) only see values produced after they subscribe — the round-trip
    // gives us a deterministic barrier for "forwarder is up".
    {
        let warmup_db = db.clone();
        let warmup = tokio::spawn(async move {
            loop {
                warmup_db
                    .produce::<ValueA>("stress::A", ValueA(SENTINEL))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        loop {
            let s = tokio::time::timeout(Duration::from_secs(2), echo_rx.recv())
                .await
                .expect("warm-up: join forwarder did not subscribe in time")
                .unwrap();
            if s.0 == SENTINEL {
                break;
            }
        }
        warmup.abort();
        let _ = warmup.await;
    }

    // Drain any remaining warm-up echoes so the burst checker sees a clean stream.
    while let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(50), echo_rx.recv()).await {
        assert_eq!(
            s.0, SENTINEL,
            "only warm-up sentinels should be in flight here"
        );
    }

    // Burst N events. The join handler yields between every receive, so the
    // bounded(64) fan-in fills up and backpressures the input forwarder. A
    // missing or broken backpressure path would deadlock here.
    let producer_db = db.clone();
    let producer_task = tokio::spawn(async move {
        for i in 0..N {
            producer_db
                .produce::<ValueA>("stress::A", ValueA(i))
                .await
                .unwrap();
        }
    });

    for expected in 0..N {
        let s = tokio::time::timeout(Duration::from_secs(5), echo_rx.recv())
            .await
            .expect("backpressured fan-in should not deadlock")
            .unwrap();
        assert_eq!(
            s.0, expected,
            "values must arrive in order under backpressure"
        );
    }

    producer_task.await.unwrap();
}
