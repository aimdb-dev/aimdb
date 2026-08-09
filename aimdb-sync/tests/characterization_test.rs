//! Characterization tests for aimdb-sync
//!
//! These tests capture the observable behavior of aimdb-sync

#![cfg(feature = "std")]

use std::sync::mpsc;
use std::{sync::Arc, thread, time::Duration};

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::{AimDbBuilderSyncExt, SyncError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[derive(Debug, Clone, PartialEq)]
struct TestData {
    id: u32,
    value: String,
}

/// Characterize sync consumer `get()` behavior
#[test]
fn test_consumer_get() {
    // Create the tokio runtime adapter, aimdb builder, configure builder
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>("test-data", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    // Create aimdb and get its handle
    let handle = builder.attach().expect("Failed to attach");

    // Create mpsc channel to set explicit synchronization
    let (ready_sender, ready_receiver) = mpsc::channel::<()>();

    // Create sync producer and consumer
    let producer = handle
        .producer::<TestData>("test-data")
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    // Instance of record type TestData to send
    let test_value = TestData {
        id: 1,
        value: "test".to_string(),
    };

    // Create sync consumer thread, send the ready signal
    let consumer_handle = thread::spawn(move || {
        ready_sender.send(()).expect("Failed to send ready signal");

        consumer.get().expect("Failed to consume")
    });

    // Clone the test value, create producer thread and receive ready signal before publishing the test value
    let val_to_send = test_value.clone();
    let producer_handle = thread::spawn(move || {
        ready_receiver
            .recv()
            .expect("Failed to receive ready signal");

        producer.set(val_to_send).expect("Failed to produce");
    });

    // Join producer thread
    producer_handle.join().unwrap();

    // Assert the consumer received the producer's published value
    match consumer_handle.join() {
        Ok(val) => assert_eq!(val, test_value),
        Err(_) => panic!("Failed to join consumer handle"),
    }

    // Shutdown aimdb cleanly
    handle.detach().expect("Failed to detach");
}

/// Characterize sync consumer `get()` receives the values in the order they were produced
#[test]
fn test_consumer_get_ordering() {
    // Create the tokio runtime adapter, aimdb builder, configure builder
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>("test-data", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    // Create aimdb and get its handle
    let handle = builder.attach().expect("Failed to attach");

    let producer = handle
        .producer::<TestData>("test-data")
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    // Store the values to send in a vector
    let mut test_val_arr: Vec<TestData> = Vec::new();

    for i in 0..10 {
        let test_val = TestData {
            id: i,
            value: format!("test-val-{i}"),
        };

        test_val_arr.push(test_val.clone());
    }

    let test_val_arr_clone = test_val_arr.clone();

    // Create producer thread and send the data
    let producer_handle = thread::spawn(move || {
        for data in test_val_arr {
            producer.set(data).expect("Failed to produce");
        }
    });

    // Join producer thread
    producer_handle.join().unwrap();

    // Create consumer thread and receive the values
    let consumer_handle = thread::spawn(move || {
        let mut received_values: Vec<TestData> = Vec::new();

        for _ in 0..10 {
            received_values.push(consumer.get().expect("Failed to consume"));
        }

        received_values
    });

    // Assert the values are received fully and in the same order they were sent
    match consumer_handle.join() {
        Ok(val) => {
            assert_eq!(val, test_val_arr_clone)
        }
        Err(_) => panic!("Failed to join consumer handle"),
    }

    handle.detach().expect("Failed to detach");
}

/// Characterize a blocked sync consumer returns `RuntimeShutdown` rather than hanging forever when the handle is detached
#[test]
fn test_consumer_shutdown() {
    // Create the tokio runtime adapter, aimdb builder, configure builder
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>("test-data", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    // Create aimdb and get its handle
    let handle = builder.attach().expect("Failed to attach");

    // Create mpsc channel to set explicit synchronization
    let (ready_sender, ready_receiver) = mpsc::channel::<()>();

    // Create sync consumer
    let consumer = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    // Create consumer thread and wait with `get()`
    let consumer_handle = thread::spawn(move || {
        ready_sender
            .send(())
            .expect("Failed to send the ready signal");
        consumer.get()
    });

    // Shutdown aimdb cleanly, before consumer received any value
    ready_receiver
        .recv()
        .expect("Failed to receive ready signal");

    handle.detach().expect("Failed to detach");

    // Assert the consumer ends with `RuntimeShutdown` error instead of hanging forever
    match consumer_handle.join() {
        Ok(val) => assert!(matches!(val, Err(SyncError::RuntimeShutdown))),
        Err(_) => panic!("Failed to join consumer handle"),
    }
}

/// Characterize sync consumer `get_with_timeout()` behavior
#[test]
fn test_consumer_get_with_timeout() {
    // Create the tokio runtime adapter, aimdb builder, configure builder
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>("test-data", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    // Create aimdb and get its handle
    let handle = builder.attach().expect("Failed to attach");

    // Create consumer thread, send a ready signal, and wait to receive a value for 100ms
    let consumer_timeout = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    let consumer_handle =
        thread::spawn(move || consumer_timeout.get_with_timeout(Duration::from_millis(100)));

    match consumer_handle.join() {
        Ok(val) => assert!(matches!(val, Err(SyncError::GetTimeout))),
        Err(_) => panic!("Failed to join consumer handle"),
    }

    // Create the ready-signal channel
    let (ready_sender, ready_receiver) = mpsc::channel::<()>();

    // Create sync producer and consumer to characterize successful get
    let producer_send = handle
        .producer::<TestData>("test-data")
        .expect("Failed to create producer");
    let consumer_get = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    // Create consumer thread, send a ready signal, and wait to receive a value for 100ms
    let consumer_get_handle = thread::spawn(move || {
        ready_sender.send(()).expect("Failed to send ready signal");

        consumer_get
            .get_with_timeout(Duration::from_millis(100))
            .expect("Failed to consume")
    });

    let test_send_value = TestData {
        id: 2,
        value: "test-send-before-timeout".to_string(),
    };

    // Create producer thread and send the test value after receiving ready signal
    let val_to_send = test_send_value.clone();
    let producer_handle = thread::spawn(move || {
        ready_receiver
            .recv()
            .expect("Failed to receive ready signal");

        producer_send.set(val_to_send).expect("Failed to produce");
    });

    // Join producer thread
    producer_handle.join().unwrap();

    // Assert consumer received the value producer sent
    match consumer_get_handle.join() {
        Ok(val) => assert_eq!(val, test_send_value),
        Err(_) => panic!("Failed to join consumer handle"),
    }

    // Shutdown aimdb cleanly
    handle.detach().expect("Failed to detach");
}

/// Characterize sync consumer `try_get()` behavior
#[test]
fn test_consumer_try_get() {
    // Create the tokio runtime adapter, aimdb builder, configure builder
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<TestData>("test-data", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(|_ctx, _consumer| async move {
                // No-op tap just to satisfy validation
            });
    });

    // Create aimdb and get its handle
    let handle = builder.attach().expect("Failed to attach");

    // Create sync producer and consumer
    let producer = handle
        .producer::<TestData>("test-data")
        .expect("Failed to create producer");
    let consumer = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    let test_value = TestData {
        id: 1,
        value: String::from("test"),
    };

    let val_to_send = test_value.clone();

    // Create producer thread, send the test value
    let producer_handle = thread::spawn(move || {
        producer.set(val_to_send).expect("Failed to produce");
    });

    // Join the producer thread
    producer_handle.join().unwrap();

    // Create consumer thread and try get the value
    let consumer_handle = thread::spawn(move || consumer.try_get().expect("Failed to consume"));

    // Assert the consumer received the value correctly
    match consumer_handle.join() {
        Ok(val) => assert_eq!(val, test_value),
        Err(_) => panic!("Failed to join consumer handle"),
    }

    // Create sync consumer thread
    let consumer_no_val = handle
        .consumer::<TestData>("test-data")
        .expect("Failed to create consumer");

    // Create consumer thread and try get the value
    let consumer_no_val_handler = thread::spawn(move || consumer_no_val.try_get());

    // Assert that consumer returned `GetTimeout` error because no value was produced
    match consumer_no_val_handler.join() {
        Ok(val) => assert!(matches!(val, Err(SyncError::GetTimeout))),
        Err(_) => panic!("Failed to join consumer handle"),
    }

    // Shutdown aimdb cleanly
    handle.detach().expect("Failed to detach");
}
