//! Multi-Instance Record Tests
//!
//! Tests for Issue #60: RecordId + RecordKey architecture
//! Validates the ability to have multiple records of the same Rust type
//! with different keys.

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

/// Simple temperature type used for testing
#[derive(Clone, Debug, PartialEq)]
struct Temperature {
    celsius: f32,
}

/// Configuration type used for testing
#[derive(Clone, Debug, PartialEq)]
struct AppConfig {
    debug: bool,
    name: String,
}

/// Test: Multiple records of the same type with different keys
#[tokio::test]
async fn test_multi_instance_same_type() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register two Temperature records with different keys
    builder.configure::<Temperature>("sensors.indoor", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("sensors.outdoor", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Produce to each record separately
    db.produce_by_key::<Temperature>("sensors.indoor", Temperature { celsius: 22.0 })
        .await
        .unwrap();

    db.produce_by_key::<Temperature>("sensors.outdoor", Temperature { celsius: -5.0 })
        .await
        .unwrap();

    // Verify we can resolve both keys
    let indoor_id = db.resolve_key("sensors.indoor").unwrap();
    let outdoor_id = db.resolve_key("sensors.outdoor").unwrap();

    // IDs should be different
    assert_ne!(indoor_id, outdoor_id);

    // Verify records_of_type returns both
    let temp_records = db.records_of_type::<Temperature>();
    assert_eq!(temp_records.len(), 2);
}

/// Test: Key-based producer works correctly
#[tokio::test]
async fn test_producer_by_key() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("sensor.a", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("sensor.b", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Get key-bound producers
    let producer_a = db.producer_by_key::<Temperature>("sensor.a");
    let producer_b = db.producer_by_key::<Temperature>("sensor.b");

    // Verify keys are bound correctly
    assert_eq!(producer_a.key(), "sensor.a");
    assert_eq!(producer_b.key(), "sensor.b");

    // Produce values
    producer_a
        .produce(Temperature { celsius: 10.0 })
        .await
        .unwrap();
    producer_b
        .produce(Temperature { celsius: 20.0 })
        .await
        .unwrap();
}

/// Test: Key-based consumer works correctly
#[tokio::test]
async fn test_consumer_by_key() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("zone.north", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("zone.south", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Get key-bound consumers
    let consumer_north = db.consumer_by_key::<Temperature>("zone.north");
    let consumer_south = db.consumer_by_key::<Temperature>("zone.south");

    // Verify keys are bound correctly
    assert_eq!(consumer_north.key(), "zone.north");
    assert_eq!(consumer_south.key(), "zone.south");

    // Subscribe should work
    let _reader_north = consumer_north.subscribe().unwrap();
    let _reader_south = consumer_south.subscribe().unwrap();
}

/// Test: Type-based lookup returns AmbiguousType error with multiple instances
#[tokio::test]
async fn test_ambiguous_type_error() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register two records of the same type
    builder.configure::<Temperature>("temp.1", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("temp.2", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Legacy type-based produce should fail with AmbiguousType
    let result = db.produce(Temperature { celsius: 25.0 }).await;

    match result {
        Err(DbError::AmbiguousType { count, .. }) => {
            assert_eq!(count, 2);
        }
        other => panic!("Expected AmbiguousType error, got: {:?}", other),
    }
}

/// Test: Type-based lookup works when only one instance exists
#[tokio::test]
async fn test_single_instance_type_lookup() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register only one Temperature record
    builder.configure::<Temperature>("single.temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Legacy type-based produce should work
    db.produce(Temperature { celsius: 30.0 }).await.unwrap();

    // Legacy type-based subscribe should work
    let _reader = db.subscribe::<Temperature>().unwrap();
}

/// Test: Key not found error
#[tokio::test]
async fn test_key_not_found_error() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("existing.key", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Try to produce to non-existent key
    let result = db
        .produce_by_key::<Temperature>("nonexistent.key", Temperature { celsius: 0.0 })
        .await;

    match result {
        Err(DbError::RecordKeyNotFound { key }) => {
            assert_eq!(key, "nonexistent.key");
        }
        other => panic!("Expected RecordKeyNotFound error, got: {:?}", other),
    }
}

/// Test: Type mismatch error when accessing with wrong type
#[tokio::test]
async fn test_type_mismatch_error() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register a Temperature record
    builder.configure::<Temperature>("sensor.temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Try to produce AppConfig to a Temperature record
    let result = db
        .produce_by_key::<AppConfig>(
            "sensor.temp",
            AppConfig {
                debug: true,
                name: "test".to_string(),
            },
        )
        .await;

    match result {
        Err(DbError::TypeMismatch { record_id, .. }) => {
            // record_id should be 0 (first registered)
            assert_eq!(record_id, 0);
        }
        other => panic!("Expected TypeMismatch error, got: {:?}", other),
    }
}

/// Test: Duplicate key error (registering same key twice panics at configure time)
///
/// Note: The current implementation panics during configure() rather than
/// returning an error during build(). This is a fail-fast design choice.
#[tokio::test]
#[should_panic(expected = "already registered with different type")]
async fn test_duplicate_key_error() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register first record
    builder.configure::<Temperature>("shared.key", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    // Try to register second record with same key - should panic
    builder.configure::<AppConfig>("shared.key", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
}

/// Test: RecordId remains stable and can be used for O(1) access
#[tokio::test]
async fn test_record_id_stability() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("first", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<AppConfig>("second", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("third", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // RecordIds should be assigned in registration order
    let id_first = db.resolve_key("first").unwrap();
    let id_second = db.resolve_key("second").unwrap();
    let id_third = db.resolve_key("third").unwrap();

    assert_eq!(id_first.raw(), 0);
    assert_eq!(id_second.raw(), 1);
    assert_eq!(id_third.raw(), 2);
}

/// Test: records_of_type introspection
#[tokio::test]
async fn test_records_of_type_introspection() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register 3 Temperature and 1 AppConfig
    builder.configure::<Temperature>("temp.a", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("temp.b", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<AppConfig>("config", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure::<Temperature>("temp.c", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let db = builder.build().await.unwrap();

    // Should find 3 Temperature records
    let temp_ids = db.records_of_type::<Temperature>();
    assert_eq!(temp_ids.len(), 3);

    // Should find 1 AppConfig record
    let config_ids = db.records_of_type::<AppConfig>();
    assert_eq!(config_ids.len(), 1);

    // Unregistered type should return empty slice
    let other_ids = db.records_of_type::<String>();
    assert!(other_ids.is_empty());
}
