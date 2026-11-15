//! Multi-Instance Record Tests
//!
//! Validates the RecordId + RecordKey architecture: the ability to have multiple records of the same Rust type
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
    let key_sensors_indoor_1 = aimdb_core::Name::<Temperature>::new("sensors.indoor");
    let key_sensors_outdoor_2 = aimdb_core::Name::<Temperature>::new("sensors.outdoor");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register two Temperature records with different keys
    builder.configure(key_sensors_indoor_1, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_sensors_outdoor_2, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Produce to each record separately
    let record_key_1 = aimdb_core::Name::<Temperature>::new("sensors.indoor");
    db.produce(record_key_1, Temperature { celsius: 22.0 })
        .unwrap();

    let record_key_2 = aimdb_core::Name::<Temperature>::new("sensors.outdoor");
    db.produce(record_key_2, Temperature { celsius: -5.0 })
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
async fn test_producer() {
    let key_sensor_a_3 = aimdb_core::Name::<Temperature>::new("sensor.a");
    let key_sensor_b_4 = aimdb_core::Name::<Temperature>::new("sensor.b");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_sensor_a_3, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_sensor_b_4, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Key-bound producers resolve the typed record up front.
    let producer_a = db.producer(key_sensor_a_3).unwrap();
    let producer_b = db.producer(key_sensor_b_4).unwrap();

    // Produce values
    producer_a.produce(Temperature { celsius: 10.0 });
    producer_b.produce(Temperature { celsius: 20.0 });
}

/// Test: Key-based consumer works correctly
#[tokio::test]
async fn test_consumer() {
    let key_zone_north_5 = aimdb_core::Name::<Temperature>::new("zone.north");
    let key_zone_south_6 = aimdb_core::Name::<Temperature>::new("zone.south");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_zone_north_5, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_zone_south_6, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Key-bound consumers resolve the typed record up front.
    let consumer_north = db.consumer(key_zone_north_5).unwrap();
    let consumer_south = db.consumer(key_zone_south_6).unwrap();

    // Subscribe should work
    let _reader_north = consumer_north.subscribe();
    let _reader_south = consumer_south.subscribe();
}

/// Test: Single instance key-based lookup works
#[tokio::test]
async fn test_single_instance_key_lookup() {
    let key_single_temp_7 = aimdb_core::Name::<Temperature>::new("single.temp");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register only one Temperature record
    builder.configure(key_single_temp_7, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Key-based produce should work
    let record_key_3 = aimdb_core::Name::<Temperature>::new("single.temp");
    db.produce(record_key_3, Temperature { celsius: 30.0 })
        .unwrap();

    // Key-based subscribe should work
    let record_key_4 = aimdb_core::Name::<Temperature>::new("single.temp");
    let _reader = db.subscribe(record_key_4).unwrap();
}

/// Test: Key not found error
#[tokio::test]
async fn test_key_not_found_error() {
    let key_existing_key_8 = aimdb_core::Name::<Temperature>::new("existing.key");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_existing_key_8, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Try to produce to non-existent key
    let record_key_5 = aimdb_core::Name::<Temperature>::new("nonexistent.key");
    let result = db.produce(record_key_5, Temperature { celsius: 0.0 });

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
    let key_sensor_temp_9 = aimdb_core::Name::<Temperature>::new("sensor.temp");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register a Temperature record
    builder.configure(key_sensor_temp_9, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    // Try to produce AppConfig to a Temperature record
    let record_key_6 = aimdb_core::Name::<AppConfig>::new("sensor.temp");
    let result = db.produce(
        record_key_6,
        AppConfig {
            debug: true,
            name: "test".to_string(),
        },
    );

    match result {
        Err(DbError::TypeMismatch { record_id, .. }) => {
            // record_id should be 0 (first registered)
            assert_eq!(record_id, 0);
        }
        other => panic!("Expected TypeMismatch error, got: {:?}", other),
    }
}

/// Test: Re-registering a key with a different type is collected by
/// configure() and reported from build() (builder methods never
/// panic on user mistakes).
#[tokio::test]
async fn test_duplicate_key_error() {
    let key_shared_key_10 = aimdb_core::Name::<Temperature>::new("shared.key");
    let key_shared_key_11 = aimdb_core::Name::<AppConfig>::new("shared.key");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register first record
    builder.configure(key_shared_key_10, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    // Re-register the same key with a different type — recorded, not panicked
    builder.configure(key_shared_key_11, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let Err(DbError::InvalidConfiguration { errors }) = builder.build().await else {
        panic!("build() must fail with InvalidConfiguration");
    };
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].record_key, "shared.key");
    assert!(errors[0].message.contains("different type"));
}

/// Test: conflicting writers (.source() + .transform()) on one record are
/// validated once, in build(), and reported with the record key attached
/// (the setters only catch same-stage duplicates).
#[tokio::test]
async fn test_conflicting_writers_reported_from_build() {
    let key_t_input_12 = aimdb_core::Name::<Temperature>::new("t.input");
    let key_t_conflicted_13 = aimdb_core::Name::<Temperature>::new("t.conflicted");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_t_input_12, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_t_conflicted_13, |reg| {
        let key_t_input_14 = aimdb_core::Name::<Temperature>::new("t.input");
        reg.buffer(BufferCfg::SingleLatest)
            .source(|_ctx, _producer| async move {})
            .transform(key_t_input_14, |b| b.map(|t: &Temperature| Some(t.clone())));
    });

    let Err(DbError::InvalidConfiguration { errors }) = builder.build().await else {
        panic!("build() must fail with InvalidConfiguration");
    };
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].record_key, "t.conflicted");
    assert!(
        errors[0].message.contains("conflicting writers"),
        "got: {}",
        errors[0].message
    );
    assert!(errors[0].message.contains("source + transform"));
}

/// Test: RecordId remains stable and can be used for O(1) access
#[tokio::test]
async fn test_record_id_stability() {
    let key_first_15 = aimdb_core::Name::<Temperature>::new("first");
    let key_second_16 = aimdb_core::Name::<AppConfig>::new("second");
    let key_third_17 = aimdb_core::Name::<Temperature>::new("third");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_first_15, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_second_16, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_third_17, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

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
    let key_temp_a_18 = aimdb_core::Name::<Temperature>::new("temp.a");
    let key_temp_b_19 = aimdb_core::Name::<Temperature>::new("temp.b");
    let key_config_20 = aimdb_core::Name::<AppConfig>::new("config");
    let key_temp_c_21 = aimdb_core::Name::<Temperature>::new("temp.c");
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register 3 Temperature and 1 AppConfig
    builder.configure(key_temp_a_18, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_temp_b_19, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_config_20, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    builder.configure(key_temp_c_21, |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

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
