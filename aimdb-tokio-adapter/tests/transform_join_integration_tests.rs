//! Integration tests for `transform_join` (multi-input reactive transform).
//!
//! Scenario: two u32 inputs A and B, one output Sum = a + b emitted whenever
//! both have been seen at least once. Drives a fixed sequence and asserts that
//! the output values are produced in the expected order.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::transform::JoinTrigger;
use aimdb_core::{AimDbBuilder, Producer};
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
// Join handler — named fn required for the 'static Future bound
//
// State = (last_a, last_b). Update synchronously, then clone the producer and
// copy the scalar sum into an owned async block.
// ---------------------------------------------------------------------------

fn sum_handler(
    trigger: JoinTrigger,
    state: &mut (Option<u32>, Option<u32>),
    producer: &Producer<Sum, TokioAdapter>,
) -> Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>> {
    match trigger.index() {
        0 => state.0 = trigger.as_input::<ValueA>().copied().map(|v| v.0),
        1 => state.1 = trigger.as_input::<ValueB>().copied().map(|v| v.0),
        _ => {}
    }
    match (state.0, state.1) {
        (Some(a), Some(b)) => {
            let p = producer.clone();
            let sum = a + b;
            Box::pin(async move {
                let _ = p.produce(Sum(sum)).await;
            })
        }
        _ => Box::pin(async {}),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transform_join_produces_sum_on_both_inputs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<ValueA>("test::A", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    builder.configure::<ValueB>("test::B", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    builder.configure::<Sum>("test::Sum", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform_join(|b| {
                b.input::<ValueA>("test::A")
                    .input::<ValueB>("test::B")
                    .with_state((None::<u32>, None::<u32>))
                    .on_trigger(sum_handler)
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
