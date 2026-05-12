use aimdb_core::{
    buffer::BufferCfg,
    builder::{AimDbBuilder, Source, Tap},
};
use aimdb_tokio_adapter::TokioRuntime;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn source_and_tap_single_latest_delivers_value() {
    let db = AimDbBuilder::new::<TokioRuntime>().build();
    let source: Source<f32> = db.source("test_channel", BufferCfg::SingleLatest);
    let tap: Tap<f32> = db.tap("test_channel");

    source.send(42.0).await;
    let value = tap.recv().await;
    assert_eq!(value, Some(42.0));
}

#[tokio::test]
async fn single_latest_overwrites_previous_value() {
    let db = AimDbBuilder::new::<TokioRuntime>().build();
    let source: Source<f32> = db.source("overwrite_ch", BufferCfg::SingleLatest);
    let tap: Tap<f32> = db.tap("overwrite_ch");

    source.send(1.0).await;
    source.send(2.0).await;
    source.send(3.0).await;

    sleep(Duration::from_millis(10)).await;

    let value = tap.recv().await;
    assert_eq!(value, Some(3.0));
}

#[tokio::test]
async fn tap_receives_multiple_updates_in_order() {
    let db = AimDbBuilder::new::<TokioRuntime>().build();
    let source: Source<f32> = db.source("ordered_ch", BufferCfg::SingleLatest);
    let tap: Tap<f32> = db.tap("ordered_ch");

    let mut received = Vec::new();
    for v in [0.1_f32, 0.5, 0.9] {
        source.send(v).await;
        if let Some(val) = tap.recv().await {
            received.push(val);
        }
    }

    assert_eq!(received.len(), 3);
    assert!((received[2] - 0.9).abs() < f32::EPSILON);
}
