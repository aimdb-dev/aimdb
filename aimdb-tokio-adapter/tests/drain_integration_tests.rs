//! Integration tests for record.drain via AimX protocol
//!
//! Tests the full record history drain pipeline:
//! - AimDB server with remote access enabled
//! - AimxClient connecting via Unix domain socket
//! - record.drain protocol method (cold start, accumulation, limits, overflow)
//!
//! These tests exercise: handler.rs dispatch → ConnectionState.drain_readers →
//! JsonReaderAdapter.try_recv_json() → TokioBufferReader.try_recv()

use aimdb_client::{AimxClient, DrainResponse};
use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Test record type: simple temperature reading
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Temperature {
    celsius: f64,
    sensor_id: String,
}

/// Test record type: simple counter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Counter {
    value: u64,
}

/// Helper: generate a unique socket path for test isolation
fn test_socket_path(test_name: &str) -> String {
    format!(
        "/tmp/aimdb-test-drain-{}-{}.sock",
        test_name,
        std::process::id()
    )
}

/// Helper: set up a test AimDB server with remote access
async fn setup_test_server(socket_path: &str) -> aimdb_core::AimDb<TokioAdapter> {
    let _ = std::fs::remove_file(socket_path);

    let adapter = Arc::new(TokioAdapter);

    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(SecurityPolicy::read_write())
        .max_connections(10);

    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_remote_access(remote_config);

    // SpmcRing for drain testing (capacity 20)
    builder.configure::<Temperature>("test::Temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 20 })
            .with_remote_access();
    });

    // SingleLatest for drain behavior comparison
    builder.configure::<Counter>("test::Counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    builder.build().await.unwrap()
}

/// Helper: set up server with a small ring to test overflow
async fn setup_small_ring_server(socket_path: &str) -> aimdb_core::AimDb<TokioAdapter> {
    let _ = std::fs::remove_file(socket_path);

    let adapter = Arc::new(TokioAdapter);

    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(SecurityPolicy::read_write())
        .max_connections(10);

    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_remote_access(remote_config);

    builder.configure::<Counter>("test::Counter", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 4 })
            .with_remote_access();
    });

    builder.build().await.unwrap()
}

/// Helper: set up server with a record that does NOT have remote access
async fn setup_no_remote_access_server(socket_path: &str) -> aimdb_core::AimDb<TokioAdapter> {
    let _ = std::fs::remove_file(socket_path);

    let adapter = Arc::new(TokioAdapter);

    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(SecurityPolicy::read_write())
        .max_connections(10);

    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_remote_access(remote_config);

    // Register WITHOUT .with_remote_access()
    builder.configure::<Counter>("test::Counter", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 });
    });

    builder.build().await.unwrap()
}

// ============================================================================
// Test: First drain returns empty (cold start)
// ============================================================================

#[tokio::test]
async fn test_drain_cold_start_returns_empty() {
    let socket_path = test_socket_path("cold_start");
    let db = setup_test_server(&socket_path).await;

    // Write some data BEFORE client connects
    db.produce::<Temperature>(
        "test::Temperature",
        Temperature {
            celsius: 20.0,
            sensor_id: "s1".to_string(),
        },
    )
    .await
    .unwrap();

    // Give the server time to start listening
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect client
    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // First drain creates the reader — returns empty (cold start)
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(
        response.count, 0,
        "First drain should be empty (cold start)"
    );
    assert!(response.values.is_empty());
    assert_eq!(response.record_name, "test::Temperature");

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain returns accumulated values after writes
// ============================================================================

#[tokio::test]
async fn test_drain_returns_accumulated_values() {
    let socket_path = test_socket_path("accumulated");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // First drain — cold start
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 0);

    // Write 5 values
    for i in 0..5 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: 20.0 + i as f64,
                sensor_id: format!("s{}", i),
            },
        )
        .await
        .unwrap();
    }

    // Small delay for buffer propagation
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Second drain — should return all 5 values
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 5, "Should drain all 5 accumulated values");
    assert_eq!(response.values.len(), 5);

    // Verify values are in chronological order
    for (i, val) in response.values.iter().enumerate() {
        let celsius = val["celsius"].as_f64().unwrap();
        assert!(
            (celsius - (20.0 + i as f64)).abs() < f64::EPSILON,
            "Value {} should have celsius={}, got {}",
            i,
            20.0 + i as f64,
            celsius
        );
    }

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Sequential drains — each returns only new values
// ============================================================================

#[tokio::test]
async fn test_drain_sequential_only_new_values() {
    let socket_path = test_socket_path("sequential");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start
    let _ = client.drain_record("test::Temperature").await.unwrap();

    // Write 3 values
    for i in 0..3 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: 10.0 + i as f64,
                sensor_id: format!("batch1-{}", i),
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain #1 — returns 3
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 3, "First drain should return 3 values");

    // Drain #2 — returns empty (nothing new)
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 0, "Immediate second drain should be empty");

    // Write 2 more values
    for i in 0..2 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: 30.0 + i as f64,
                sensor_id: format!("batch2-{}", i),
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain #3 — returns only the 2 new values
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(
        response.count, 2,
        "Third drain should return only 2 new values"
    );

    // Verify the values are from batch2
    let first_celsius = response.values[0]["celsius"].as_f64().unwrap();
    assert!(
        (first_celsius - 30.0).abs() < f64::EPSILON,
        "First value should be 30.0, got {}",
        first_celsius
    );

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain with limit parameter
// ============================================================================

#[tokio::test]
async fn test_drain_with_limit() {
    let socket_path = test_socket_path("limit");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start
    let _ = client.drain_record("test::Temperature").await.unwrap();

    // Write 10 values
    for i in 0..10 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: i as f64,
                sensor_id: format!("s{}", i),
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain with limit=3
    let response = client
        .drain_record_with_limit("test::Temperature", 3)
        .await
        .unwrap();
    assert_eq!(response.count, 3, "Should return exactly 3 values");

    // Verify first 3 values
    for (i, val) in response.values.iter().enumerate() {
        let celsius = val["celsius"].as_f64().unwrap();
        assert!(
            (celsius - i as f64).abs() < f64::EPSILON,
            "Value {} should be {}, got {}",
            i,
            i,
            celsius
        );
    }

    // Drain remaining — should get the other 7
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 7, "Should drain remaining 7 values");

    // First remaining value should be celsius=3
    let celsius = response.values[0]["celsius"].as_f64().unwrap();
    assert!(
        (celsius - 3.0).abs() < f64::EPSILON,
        "First remaining value should be 3.0, got {}",
        celsius
    );

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain on SingleLatest buffer — returns at most 1 value
// ============================================================================

#[tokio::test]
async fn test_drain_single_latest_at_most_one() {
    let socket_path = test_socket_path("single_latest");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start
    let _ = client.drain_record("test::Counter").await.unwrap();

    // Write 5 values — SingleLatest overwrites, only last survives
    for i in 0..5 {
        db.produce::<Counter>("test::Counter", Counter { value: i })
            .await
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain — should return at most 1 value (the latest)
    let response = client.drain_record("test::Counter").await.unwrap();
    assert!(
        response.count <= 1,
        "SingleLatest drain should return at most 1 value, got {}",
        response.count
    );

    if response.count == 1 {
        let value = response.values[0]["value"].as_u64().unwrap();
        assert_eq!(value, 4, "Should be the last written value");
    }

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain on non-existent record returns error
// ============================================================================

#[tokio::test]
async fn test_drain_nonexistent_record_error() {
    let socket_path = test_socket_path("nonexistent");
    let _db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Drain a record that doesn't exist
    let result = client.drain_record("test::DoesNotExist").await;
    assert!(result.is_err(), "Should fail for non-existent record");

    drop(client);
    drop(_db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain requires remote_access (record without .with_remote_access())
// ============================================================================

#[tokio::test]
async fn test_drain_requires_remote_access() {
    let socket_path = test_socket_path("no_remote");
    let _db = setup_no_remote_access_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Drain a record that exists but lacks .with_remote_access()
    let result = client.drain_record("test::Counter").await;
    assert!(
        result.is_err(),
        "Should fail when record lacks .with_remote_access()"
    );

    drop(client);
    drop(_db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Ring buffer overflow — drain still works after overflow
// ============================================================================

#[tokio::test]
async fn test_drain_with_ring_overflow() {
    let socket_path = test_socket_path("overflow");
    let db = setup_small_ring_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start — creates the drain reader
    let _ = client.drain_record("test::Counter").await.unwrap();

    // Write 20 values into capacity-4 ring — causes overflow
    for i in 0..20 {
        db.produce::<Counter>("test::Counter", Counter { value: i })
            .await
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain — should recover from lag and return remaining ring values
    let response = client.drain_record("test::Counter").await.unwrap();

    // After overflow + lag recovery, we get the tail values still in the ring
    assert!(
        response.count <= 4,
        "Should return at most capacity values after overflow, got {}",
        response.count
    );

    // Values should be from the end of the sequence
    if !response.values.is_empty() {
        let last_value = response.values.last().unwrap()["value"].as_u64().unwrap();
        assert!(
            last_value >= 16,
            "Last value should be near end of sequence (>=16), got {}",
            last_value
        );
    }

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Multiple records can be drained independently
// ============================================================================

#[tokio::test]
async fn test_drain_multiple_records_independent() {
    let socket_path = test_socket_path("multi_record");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start both records
    let _ = client.drain_record("test::Temperature").await.unwrap();
    let _ = client.drain_record("test::Counter").await.unwrap();

    // Write to Temperature only
    for i in 0..3 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: i as f64,
                sensor_id: "temp".to_string(),
            },
        )
        .await
        .unwrap();
    }

    // Write to Counter only
    db.produce::<Counter>("test::Counter", Counter { value: 42 })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain Temperature — should get 3
    let temp_response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(temp_response.count, 3);

    // Drain Counter — should get 1
    let counter_response = client.drain_record("test::Counter").await.unwrap();
    assert!(
        counter_response.count <= 1,
        "SingleLatest counter should have at most 1 value"
    );

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: DrainResponse structure is correct
// ============================================================================

#[tokio::test]
async fn test_drain_response_structure() {
    let socket_path = test_socket_path("response_struct");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start
    let _ = client.drain_record("test::Temperature").await.unwrap();

    // Write a value with known fields
    db.produce::<Temperature>(
        "test::Temperature",
        Temperature {
            celsius: 25.5,
            sensor_id: "test-sensor".to_string(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain and inspect response structure
    let response: DrainResponse = client.drain_record("test::Temperature").await.unwrap();

    assert_eq!(response.record_name, "test::Temperature");
    assert_eq!(response.count, 1);
    assert_eq!(response.values.len(), 1);

    // Verify JSON structure matches Temperature fields
    let val = &response.values[0];
    assert!(val.is_object());
    assert_eq!(val["celsius"], 25.5);
    assert_eq!(val["sensor_id"], "test-sensor");

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain with zero limit returns empty
// ============================================================================

#[tokio::test]
async fn test_drain_with_zero_limit() {
    let socket_path = test_socket_path("zero_limit");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start
    let _ = client.drain_record("test::Temperature").await.unwrap();

    // Write values
    for i in 0..5 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: i as f64,
                sensor_id: "s".to_string(),
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drain with limit=0 should return nothing
    let response = client
        .drain_record_with_limit("test::Temperature", 0)
        .await
        .unwrap();
    assert_eq!(response.count, 0, "Limit=0 should return empty");

    // Values should still be pending for next drain
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(response.count, 5, "All 5 values should still be available");

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}

// ============================================================================
// Test: Drain uses its own reader, independent of other buffer consumers
// ============================================================================

#[tokio::test]
async fn test_drain_independent_of_other_consumers() {
    let socket_path = test_socket_path("drain_independent");
    let db = setup_test_server(&socket_path).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client = AimxClient::connect(&socket_path).await.unwrap();

    // Cold start the drain reader
    let _ = client.drain_record("test::Temperature").await.unwrap();

    // Also subscribe via the in-process API (not the AimX client) to
    // prove the drain reader is independent of other consumers
    let mut in_process_reader = db.subscribe::<Temperature>("test::Temperature").unwrap();

    // Write values
    for i in 0..3 {
        db.produce::<Temperature>(
            "test::Temperature",
            Temperature {
                celsius: 100.0 + i as f64,
                sensor_id: "test".to_string(),
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // In-process reader consumes all 3
    for _ in 0..3 {
        let val = in_process_reader.recv().await.unwrap();
        assert!(val.celsius >= 100.0);
    }

    // Drain should ALSO return all 3 values (independent reader)
    let response = client.drain_record("test::Temperature").await.unwrap();
    assert_eq!(
        response.count, 3,
        "Drain should return 3 values independent of in-process consumer"
    );

    drop(client);
    drop(db);
    let _ = std::fs::remove_file(&socket_path);
}
