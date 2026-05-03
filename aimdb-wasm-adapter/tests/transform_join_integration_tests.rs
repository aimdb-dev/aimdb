//! WASM integration tests for `transform_join` (multi-input reactive transform).
//!
//! Same scenario as the Tokio integration test: two u32 inputs A and B, one
//! output Sum = a + b, emitted whenever both inputs have been seen at least once.
//!
//! Run with: wasm-pack test --headless --chrome (or --firefox)

use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
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
