//! Integration coverage for `SyncProducer::set_value` (feature `data-contracts`,
//! design 041 §3.4): construct via `Settable::set`, produce, and consume
//! end-to-end through the real sync bridge.

#![cfg(feature = "data-contracts")]

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_data_contracts::{SchemaType, Settable};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
struct Temperature {
    celsius: f32,
    timestamp: u64,
}

impl SchemaType for Temperature {
    const NAME: &'static str = "sync_settable.temperature";
}

impl Settable for Temperature {
    type Value = f32;

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Temperature {
            celsius: value,
            timestamp,
        }
    }
}

#[test]
fn set_value_constructs_produces_and_is_consumed() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Temperature>("temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {});
    });

    let handle = builder.attach().expect("failed to attach");
    let producer = handle
        .producer::<Temperature>("temperature")
        .expect("failed to create producer");
    let consumer = handle
        .consumer::<Temperature>("temperature")
        .expect("failed to create consumer");

    producer.set_value(22.5).expect("set_value should succeed");

    let received = consumer
        .get_with_timeout(Duration::from_secs(2))
        .expect("failed to consume");

    assert_eq!(received.celsius, 22.5);
    assert!(received.timestamp > 0, "timestamp should be stamped");

    handle.detach().expect("failed to detach");
}

#[test]
fn try_set_value_is_non_blocking_and_produces() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Temperature>("temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {});
    });

    let handle = builder.attach().expect("failed to attach");
    let producer = handle
        .producer::<Temperature>("temperature")
        .expect("failed to create producer");
    let consumer = handle
        .consumer::<Temperature>("temperature")
        .expect("failed to create consumer");

    producer
        .try_set_value(18.0)
        .expect("try_set_value should succeed");

    thread::sleep(Duration::from_millis(100));

    let received = consumer
        .get_with_timeout(Duration::from_secs(2))
        .expect("failed to consume");
    assert_eq!(received.celsius, 18.0);

    handle.detach().expect("failed to detach");
}

#[test]
fn set_value_at_stamps_the_explicit_timestamp() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Temperature>("temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {});
    });

    let handle = builder.attach().expect("failed to attach");
    let producer = handle
        .producer::<Temperature>("temperature")
        .expect("failed to create producer");
    let consumer = handle
        .consumer::<Temperature>("temperature")
        .expect("failed to create consumer");

    // Explicit timestamp, not wall-clock — deterministic regardless of when the
    // test runs (replay/testing use case, design 041 §3.4).
    producer
        .set_value_at(22.5, 1_700_000_000_000)
        .expect("set_value_at should succeed");

    let received = consumer
        .get_with_timeout(Duration::from_secs(2))
        .expect("failed to consume");

    assert_eq!(
        received,
        Temperature {
            celsius: 22.5,
            timestamp: 1_700_000_000_000,
        }
    );
}

#[test]
fn settable_set_is_a_pure_deterministic_constructor() {
    let a = Temperature::set(22.5, 1_700_000_000_000);
    let b = Temperature::set(22.5, 1_700_000_000_000);
    assert_eq!(a, b);
}
