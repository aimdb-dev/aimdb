//! WASM integration tests for `transform_join` (multi-input reactive transform).
//!
//! Same scenario as the Tokio integration test: two u32 inputs A and B, one
//! output Sum = a + b, emitted whenever both inputs have been seen at least once.
//!
//! Run with: wasm-pack test --headless --chrome (or --firefox)

use std::pin::Pin;
use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::transform::JoinTrigger;
use aimdb_core::{AimDbBuilder, Producer};
use aimdb_wasm_adapter::{WasmAdapter, WasmRecordRegistrarExt};
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

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
// Join handler
// ---------------------------------------------------------------------------

fn sum_handler(
    trigger: JoinTrigger,
    state: &mut (Option<u32>, Option<u32>),
    producer: &Producer<Sum, WasmAdapter>,
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
// Test
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn transform_join_produces_sum_on_both_inputs() {
    let runtime = Arc::new(WasmAdapter);
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
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL))
        .await
        .unwrap();

    // A=1, B=10 → Sum=11
    db.produce::<ValueA>("test::A", ValueA(1)).await.unwrap();
    db.produce::<ValueB>("test::B", ValueB(10)).await.unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.0, 11, "expected 1+10=11");

    // A=2 → Sum=12 (B stays 10)
    db.produce::<ValueA>("test::A", ValueA(2)).await.unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.0, 12, "expected 2+10=12");

    // B=20 → Sum=22 (A stays 2)
    db.produce::<ValueB>("test::B", ValueB(20)).await.unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.0, 22, "expected 2+20=22");
}
