//! Integration tests for aimdb-sync
//!
//! These tests verify end-to-end functionality of the synchronous API wrapper.

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, DbError};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestData {
    id: u32,
    value: String,
}

/// Test basic producer-consumer flow
#[test]
fn test_basic_producer_consumer() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    // Create producer and consumer
    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer");

    // Produce a value
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };
    producer.set(test_value.clone()).expect("Failed to produce");

    // Give time for async propagation
    thread::sleep(Duration::from_millis(100));

    // Consume the value (use timeout to avoid hanging)
    let received = consumer
        .get_with_timeout(Duration::from_secs(2))
        .expect("Failed to consume");
    assert_eq!(received, test_value);

    handle.detach().expect("Failed to detach");
}

/// Test multiple producers and consumers
#[test]
fn test_multi_threaded_producer_consumer() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    // Create multiple consumers
    let consumer1 = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer 1");
    let consumer2 = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer 2");

    // Spawn consumer threads
    let c1_handle = thread::spawn(move || {
        let mut received = Vec::new();
        for _ in 0..10 {
            if let Ok(data) = consumer1.get() {
                received.push(data);
            }
        }
        received
    });

    let c2_handle = thread::spawn(move || {
        let mut received = Vec::new();
        for _ in 0..10 {
            if let Ok(data) = consumer2.get() {
                received.push(data);
            }
        }
        received
    });

    // Give consumers time to start
    thread::sleep(Duration::from_millis(50));

    // Create multiple producers
    let producer1 = handle
        .producer::<TestData>()
        .expect("Failed to create producer 1");
    let producer2 = producer1.clone();

    let p1_handle = thread::spawn(move || {
        for i in 0..10 {
            let data = TestData {
                id: i,
                value: format!("producer1-{}", i),
            };
            producer1.set(data).expect("Failed to produce");
        }
    });

    let p2_handle = thread::spawn(move || {
        for i in 10..20 {
            let data = TestData {
                id: i,
                value: format!("producer2-{}", i),
            };
            producer2.set(data).expect("Failed to produce");
        }
    });

    // Wait for all threads
    p1_handle.join().unwrap();
    p2_handle.join().unwrap();
    let c1_data = c1_handle.join().unwrap();
    let c2_data = c2_handle.join().unwrap();

    // Verify consumers received data
    assert_eq!(c1_data.len(), 10);
    assert_eq!(c2_data.len(), 10);

    handle.detach().expect("Failed to detach");
}

/// Test timeout operations
#[test]
fn test_timeout_operations() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer");

    // Test get_timeout on empty buffer (should timeout)
    let result = consumer.get_with_timeout(Duration::from_millis(100));
    assert!(matches!(result, Err(DbError::GetTimeout)));

    // Produce a value
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };
    producer
        .set_with_timeout(test_value.clone(), Duration::from_secs(1))
        .expect("Failed to produce with timeout");

    // Give more time for the value to propagate through the async pipeline
    thread::sleep(Duration::from_millis(200));

    // Get with timeout (should succeed)
    let received = consumer
        .get_with_timeout(Duration::from_secs(2))
        .expect("Failed to consume with timeout");
    assert_eq!(received, test_value);

    handle.detach().expect("Failed to detach");
}

/// Test non-blocking operations
#[test]
fn test_non_blocking_operations() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer");

    // Try get on empty buffer (should fail)
    let result = consumer.try_get();
    assert!(matches!(result, Err(DbError::GetTimeout)));

    // Try set (should succeed immediately)
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };
    producer
        .try_set(test_value.clone())
        .expect("Failed to try_set");

    // Use blocking get to ensure we receive the value
    // (try_get is inherently racy in this test scenario)
    let received = consumer
        .get_with_timeout(Duration::from_secs(1))
        .expect("Failed to receive value");
    assert_eq!(received, test_value);

    // Now try_get on empty buffer again (should fail)
    let result = consumer.try_get();
    assert!(matches!(result, Err(DbError::GetTimeout)));

    handle.detach().expect("Failed to detach");
}

/// Test graceful shutdown
#[test]
fn test_graceful_shutdown() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");

    // Produce some values
    for i in 0..5 {
        let data = TestData {
            id: i,
            value: format!("value-{}", i),
        };
        producer.set(data).expect("Failed to produce");
    }

    // Detach should succeed
    handle.detach().expect("Failed to detach");
}

/// Test detach with timeout
#[test]
fn test_detach_with_timeout() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    // Detach with timeout should succeed quickly
    handle
        .detach_timeout(Duration::from_secs(5))
        .expect("Failed to detach with timeout");
}

/// Test error handling - runtime shutdown
#[test]
fn test_runtime_shutdown_error() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer");

    // Shut down the runtime
    handle.detach().expect("Failed to detach");

    // Operations should now fail with RuntimeShutdown
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };

    let result = producer.set(test_value);
    assert!(matches!(result, Err(DbError::RuntimeShutdown)));

    let result = consumer.get_with_timeout(Duration::from_millis(100));
    assert!(matches!(
        result,
        Err(DbError::RuntimeShutdown) | Err(DbError::GetTimeout)
    ));
}

/// Test buffer semantics - SPMC Ring
#[test]
fn test_spmc_ring_semantics() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 5 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer1 = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer 1");
    let consumer2 = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer 2");

    // Produce multiple values
    for i in 0..5 {
        let data = TestData {
            id: i,
            value: format!("value-{}", i),
        };
        producer.set(data).expect("Failed to produce");
    }

    // Give time for values to propagate
    thread::sleep(Duration::from_millis(100));

    // Both consumers should be able to get values independently
    let c1_data = consumer1.get().expect("Consumer 1 failed");
    let c2_data = consumer2.get().expect("Consumer 2 failed");

    // Each consumer gets their own copy
    assert_eq!(c1_data.id, 0);
    assert_eq!(c2_data.id, 0);

    handle.detach().expect("Failed to detach");
}

/// Test buffer semantics - SingleLatest
#[test]
fn test_single_latest_semantics() {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>()
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>()
        .expect("Failed to create consumer");

    // Produce first value and wait for it to propagate
    let data = TestData {
        id: 100,
        value: "initial".to_string(),
    };
    producer.set(data).expect("Failed to produce");
    thread::sleep(Duration::from_millis(100));

    // Consume first value to establish the subscription
    let first = consumer.get().expect("Failed to consume initial value");
    assert_eq!(first.id, 100);

    // Now produce multiple values quickly (target: <50ms end-to-end)
    // Use try_set() for fire-and-forget semantics - this allows queuing all values
    // without blocking, which lets SingleLatest skip intermediates
    for i in 1..=5 {
        let data = TestData {
            id: i,
            value: format!("value-{}", i),
        };
        // Use try_set() instead of set() to avoid blocking on each produce
        // This allows all values to be queued quickly, demonstrating SingleLatest semantics
        producer.try_set(data).expect("Failed to queue value");
    }

    // Wait for propagation - with <50ms target
    thread::sleep(Duration::from_millis(50));

    // Consumer should get the latest value (5)
    // With SingleLatest semantics, intermediate values (1,2,3,4) are skipped
    let received = consumer.get().expect("Failed to consume");

    assert_eq!(
        received.id, 5,
        "Expected latest value (5), got {}. SingleLatest should skip intermediate values and deliver only the most recent.",
        received.id
    );

    handle.detach().expect("Failed to detach");
}

/// Test that produce errors are properly propagated back to the sync caller
#[test]
fn test_error_propagation() {
    // Build a database WITHOUT registering TestData
    // This will cause produce() to fail with RecordNotFound
    let adapter = Arc::new(TokioAdapter);
    let builder = AimDbBuilder::new().runtime(adapter);

    let handle = builder.attach().expect("Failed to attach");

    // Create a producer for an unregistered type
    // Note: producer creation succeeds, but set() should fail
    let producer = handle
        .producer_with_capacity::<TestData>(10)
        .expect("Failed to create producer");

    // Try to produce a value - this should fail because TestData is not registered
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };

    let result = producer.set(test_value.clone());

    // Verify the error is propagated (not silently logged)
    assert!(
        result.is_err(),
        "Expected produce to fail for unregistered type"
    );

    // Verify it's the correct error type
    match result {
        Err(DbError::RecordNotFound { .. }) => {
            // Expected error
        }
        other => panic!("Expected RecordNotFound error, got: {:?}", other),
    }

    // Test set_with_timeout also propagates errors
    let result = producer.set_with_timeout(test_value, Duration::from_millis(100));
    assert!(
        result.is_err(),
        "Expected produce to fail for unregistered type (with timeout)"
    );

    handle.detach().expect("Failed to detach");
}
