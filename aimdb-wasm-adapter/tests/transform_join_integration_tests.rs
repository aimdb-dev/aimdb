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
    let key_test_a_1 = aimdb_core::Name::<ValueA>::new("test::A");
    let key_test_b_2 = aimdb_core::Name::<ValueB>::new("test::B");
    let key_test_sum_3 = aimdb_core::Name::<Sum>::new("test::Sum");
    let runtime = Arc::new(WasmAdapter);
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
    // The join transform lives in the runner's future set; on wasm the browser
    // microtask queue is the executor, so `spawn_local` is the `tokio::spawn`.
    wasm_bindgen_futures::spawn_local(runner.run());
    let record_key_1 = aimdb_core::Name::<Sum>::new("test::Sum");
    let mut sum_rx = db.subscribe(record_key_1).unwrap();

    // Yield to let the join transform task spawn its input forwarders and subscribe.
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL))
        .await
        .unwrap();

    // A=1, B=10 → Sum=11
    let record_key_2 = aimdb_core::Name::<ValueA>::new("test::A");
    db.produce(record_key_2, ValueA(1)).unwrap();
    let record_key_3 = aimdb_core::Name::<ValueB>::new("test::B");
    db.produce(record_key_3, ValueB(10)).unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.value.0, 11, "expected 1+10=11");

    // A=2 → Sum=12 (B stays 10)
    let record_key_4 = aimdb_core::Name::<ValueA>::new("test::A");
    db.produce(record_key_4, ValueA(2)).unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.value.0, 12, "expected 2+10=12");

    // B=20 → Sum=22 (A stays 2)
    let record_key_5 = aimdb_core::Name::<ValueB>::new("test::B");
    db.produce(record_key_5, ValueB(20)).unwrap();
    let s = sum_rx.recv().await.unwrap();
    assert_eq!(s.value.0, 22, "expected 2+20=22");
}
