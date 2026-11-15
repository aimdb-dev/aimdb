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
    let key_test_a_1 = aimdb_core::Name::<ValueA>::new("test::A");
    let key_test_b_2 = aimdb_core::Name::<ValueB>::new("test::B");
    let key_test_sum_3 = aimdb_core::Name::<Sum>::new("test::Sum");
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    // SpmcRing inputs (vs SingleLatest) so that values produced before the join
    // transform's forwarders subscribe are still buffered and replayed — removes
    // a startup race where the test might otherwise need a hand-tuned barrier.
    builder.configure(key_test_a_1, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure(key_test_b_2, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure(key_test_sum_3, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform_join(|b| {
                let key_test_a_4 = aimdb_core::Name::<ValueA>::new("test::A");
                let key_test_b_5 = aimdb_core::Name::<ValueB>::new("test::B");
                b.input(key_test_a_4).input(key_test_b_5).on_triggers(
                    |mut rx, producer| async move {
                        let mut last_a: Option<u32> = None;
                        let mut last_b: Option<u32> = None;
                        while let Ok(trigger) = rx.recv().await {
                            match trigger.index() {
                                0 => last_a = trigger.as_input::<ValueA>().copied().map(|v| v.0),
                                1 => last_b = trigger.as_input::<ValueB>().copied().map(|v| v.0),
                                _ => {}
                            }
                            if let (Some(a), Some(b)) = (last_a, last_b) {
                                producer.produce(Sum(a + b));
                            }
                        }
                    },
                )
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let record_key_1 = aimdb_core::Name::<Sum>::new("test::Sum");
    let mut sum_rx = db.subscribe(record_key_1).unwrap();

    // Yield to let the join transform task spawn its input forwarders and subscribe.
    tokio::task::yield_now().await;

    // A=1, B=10 → Sum=11 (first time both are present)
    let record_key_2 = aimdb_core::Name::<ValueA>::new("test::A");
    db.produce(record_key_2, ValueA(1)).unwrap();
    let record_key_3 = aimdb_core::Name::<ValueB>::new("test::B");
    db.produce(record_key_3, ValueB(10)).unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.value.0, 11, "expected 1+10=11");

    // A=2 → Sum=12 (B stays 10)
    let record_key_4 = aimdb_core::Name::<ValueA>::new("test::A");
    db.produce(record_key_4, ValueA(2)).unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.value.0, 12, "expected 2+10=12");

    // B=20 → Sum=22 (A stays 2)
    let record_key_5 = aimdb_core::Name::<ValueB>::new("test::B");
    db.produce(record_key_5, ValueB(20)).unwrap();
    let s = tokio::time::timeout(Duration::from_secs(2), sum_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.value.0, 22, "expected 2+20=22");
}

/// Stress test for the bounded(64) Tokio fan-in channel: pushes well over 64
/// events through a single-input join while the handler intentionally yields
/// between receives. If backpressure is wired correctly, this completes
/// without deadlock and every produced value is observed in order.
#[tokio::test]
async fn transform_join_bounded_fanin_backpressure_no_deadlock() {
    let key_stress_a_6 = aimdb_core::Name::<ValueA>::new("stress::A");
    let key_stress_echo_7 = aimdb_core::Name::<Sum>::new("stress::Echo");
    const N: u32 = 200;
    const SENTINEL: u32 = u32::MAX;
    let cap = (N + 16) as usize;

    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Input/output rings sized larger than the bounded(64) fan-in so the SpmcRing
    // itself isn't the limiter — we want the bounded channel to be the bottleneck.
    builder.configure(key_stress_a_6, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: cap });
    });
    builder.configure(key_stress_echo_7, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: cap })
            .transform_join(|b| {
                let key_stress_a_8 = aimdb_core::Name::<ValueA>::new("stress::A");
                b.input(key_stress_a_8)
                    .on_triggers(|mut rx, producer| async move {
                        while let Ok(trigger) = rx.recv().await {
                            if let Some(v) = trigger.as_input::<ValueA>().copied() {
                                // Yield between receives to keep the fan-in channel
                                // pressured well above its 64-slot capacity.
                                tokio::task::yield_now().await;
                                producer.produce(Sum(v.0));
                            }
                        }
                    })
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let record_key_6 = aimdb_core::Name::<Sum>::new("stress::Echo");
    let mut echo_rx = db.subscribe(record_key_6).unwrap();

    // Warm-up: keep producing a sentinel until its echo lands. SpmcRing buffers
    // are tokio broadcast channels, so subscribers (including the join input
    // forwarder) only see values produced after they subscribe — the round-trip
    // gives us a deterministic barrier for "forwarder is up".
    {
        let warmup_db = db.clone();
        let warmup = tokio::spawn(async move {
            loop {
                let record_key_7 = aimdb_core::Name::<ValueA>::new("stress::A");
                warmup_db.produce(record_key_7, ValueA(SENTINEL)).unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        loop {
            let s = tokio::time::timeout(Duration::from_secs(2), echo_rx.recv())
                .await
                .expect("warm-up: join forwarder did not subscribe in time")
                .unwrap();
            if s.value.0 == SENTINEL {
                break;
            }
        }
        warmup.abort();
        let _ = warmup.await;
    }

    // Drain any remaining warm-up echoes so the burst checker sees a clean stream.
    while let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(50), echo_rx.recv()).await {
        assert_eq!(
            s.value.0, SENTINEL,
            "only warm-up sentinels should be in flight here"
        );
    }

    // Burst N events. The join handler yields between every receive, so the
    // bounded(64) fan-in fills up and backpressures the input forwarder. A
    // missing or broken backpressure path would deadlock here.
    let producer_db = db.clone();
    let producer_task = tokio::spawn(async move {
        for i in 0..N {
            let record_key_8 = aimdb_core::Name::<ValueA>::new("stress::A");
            producer_db.produce(record_key_8, ValueA(i)).unwrap();
        }
    });

    for expected in 0..N {
        let s = tokio::time::timeout(Duration::from_secs(5), echo_rx.recv())
            .await
            .expect("backpressured fan-in should not deadlock")
            .unwrap();
        assert_eq!(
            s.value.0, expected,
            "values must arrive in order under backpressure"
        );
    }

    producer_task.await.unwrap();
}

/// `measured_at` is time since an application-defined epoch, set by whoever
/// takes the reading. AimDB does not interpret it.
#[derive(Clone, Debug)]
struct Flow {
    measured_at: Duration,
    celsius: f32,
}

#[derive(Clone, Debug)]
struct Return {
    measured_at: Duration,
    celsius: f32,
}

#[derive(Clone, Debug)]
struct Difference {
    measured_at: Duration,
    kelvin: f32,
}

/// The pairing pattern from the `JoinBuilder::on_triggers` docs, matching on
/// application timestamps: emit only when both inputs were measured within
/// `TOLERANCE` of each other, then consume both. The 500 ms Flow is replaced by
/// the 1000 ms Flow before a Return close to it arrives, so that cycle
/// produces nothing.
#[tokio::test]
async fn transform_join_pairs_values_by_timestamp() {
    let key_pair_flow_9 = aimdb_core::Name::<Flow>::new("pair::Flow");
    let key_pair_return_10 = aimdb_core::Name::<Return>::new("pair::Return");
    let key_pair_difference_11 = aimdb_core::Name::<Difference>::new("pair::Difference");
    const TOLERANCE: Duration = Duration::from_millis(20);

    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_pair_flow_9, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure(key_pair_return_10, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure(key_pair_difference_11, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform_join(|b| {
                let key_pair_flow_12 = aimdb_core::Name::<Flow>::new("pair::Flow");
                let key_pair_return_13 = aimdb_core::Name::<Return>::new("pair::Return");
                b.input(key_pair_flow_12)
                    .input(key_pair_return_13)
                    .on_triggers(|mut rx, producer| async move {
                        let mut flow: Option<Flow> = None;
                        let mut ret: Option<Return> = None;
                        while let Ok(trigger) = rx.recv().await {
                            match trigger.index() {
                                0 => flow = trigger.as_input::<Flow>().cloned(),
                                1 => ret = trigger.as_input::<Return>().cloned(),
                                _ => {}
                            }
                            if let (Some(f), Some(r)) = (&flow, &ret) {
                                if f.measured_at.abs_diff(r.measured_at) <= TOLERANCE {
                                    producer.produce(Difference {
                                        measured_at: f.measured_at,
                                        kelvin: f.celsius - r.celsius,
                                    });
                                    flow = None;
                                    ret = None;
                                }
                            }
                        }
                    })
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let record_key_9 = aimdb_core::Name::<Difference>::new("pair::Difference");
    let mut diff_rx = db.subscribe(record_key_9).unwrap();
    tokio::task::yield_now().await;

    let ms = Duration::from_millis;
    let flow = |at, celsius| Flow {
        measured_at: ms(at),
        celsius,
    };
    let ret = |at, celsius| Return {
        measured_at: ms(at),
        celsius,
    };

    // 5 ms apart, within tolerance; the Return is the earlier of the two.
    let record_key_10 = aimdb_core::Name::<Return>::new("pair::Return");
    db.produce(record_key_10, ret(0, 45.0)).unwrap();
    let record_key_11 = aimdb_core::Name::<Flow>::new("pair::Flow");
    db.produce(record_key_11, flow(5, 60.0)).unwrap();
    let d = tokio::time::timeout(Duration::from_secs(2), diff_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((d.value.measured_at, d.value.kelvin), (ms(5), 15.0));

    // Each input is delivered in write order, so every interleaving of the
    // two inputs ends with the 1000 ms Flow and the 1012 ms Return retained
    // together; the 500 ms Flow never comes within tolerance of a Return.
    let record_key_12 = aimdb_core::Name::<Flow>::new("pair::Flow");
    db.produce(record_key_12, flow(500, 61.0)).unwrap();
    let record_key_13 = aimdb_core::Name::<Flow>::new("pair::Flow");
    db.produce(record_key_13, flow(1000, 62.0)).unwrap();
    let record_key_14 = aimdb_core::Name::<Return>::new("pair::Return");
    db.produce(record_key_14, ret(1012, 50.0)).unwrap();
    let d = tokio::time::timeout(Duration::from_secs(2), diff_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (d.value.measured_at, d.value.kelvin),
        (ms(1000), 12.0),
        "the 500 ms Flow has no Return within tolerance and must be skipped"
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(100), diff_rx.recv())
            .await
            .is_err(),
        "no output beyond the two matched pairs"
    );
}
