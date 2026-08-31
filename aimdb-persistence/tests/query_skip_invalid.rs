//! Integration tests: `query_latest` / `query_range` skip rows that fail to
//! deserialize as `T` and still return all valid rows.
//!
//! These tests exercise the `filter_map` in `AimDbQueryExt` (query_ext.rs) that
//! recovers from per-row `serde_json::from_value` failures rather than
//! propagating them as errors.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::AimDbBuilder;
use aimdb_persistence::{
    topic_matches, AimDbBuilderPersistExt, AimDbQueryExt, BoxFuture, PersistenceBackend,
    PersistenceError, QueryParams, StoredValue,
};
use aimdb_tokio_adapter::TokioAdapter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Target type used in all tests
// ---------------------------------------------------------------------------

/// A simple record type: only rows whose JSON contains `{ "celsius": <f64> }`
/// deserialize successfully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SensorReading {
    celsius: f64,
}

// ---------------------------------------------------------------------------
// Mock backend — a fixed list of StoredValues, narrowed by the record pattern
// ---------------------------------------------------------------------------

struct MockBackend {
    rows: Vec<StoredValue>,
}

impl PersistenceBackend for MockBackend {
    fn store<'a>(
        &'a self,
        _record_name: &'a str,
        _value: &'a Value,
        _timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    /// Returns the pre-configured rows the pattern matches, decided by
    /// [`topic_matches`] — the matcher [`PersistenceBackend::query`] requires,
    /// so this mock is bound by the same grammar a real backend is.
    ///
    /// `QueryParams` stays ignored: the row-level filtering under test lives in
    /// `AimDbQueryExt`, not here. The pattern does not, because a mock that
    /// ignored it would leave the whole crate unable to tell the current
    /// grammar from the prefix-glob one it replaced.
    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        _params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        let rows: Vec<StoredValue> = self
            .rows
            .iter()
            .filter(|row| topic_matches(record_pattern, &row.record_name))
            .cloned()
            .collect();
        Box::pin(async move { Ok(rows) })
    }

    fn cleanup(&self, _older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Ok(0) })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal `AimDb` backed by `mock` (no records
/// configured — we only need the Extensions TypeMap and the query path).
async fn build_db(mock: Arc<MockBackend>) -> aimdb_core::AimDb {
    let adapter = Arc::new(TokioAdapter);
    let (db, runner) = AimDbBuilder::new()
        .runtime(adapter)
        .with_persistence(
            mock as Arc<dyn PersistenceBackend>,
            // Short retention — the cleanup loop is a no-op in the mock.
            Duration::from_secs(3600),
        )
        .build()
        .await
        .expect("AimDb build failed");
    tokio::spawn(runner.run());
    db
}

/// Shared fixture: four rows where rows at indices 1 and 3 cannot be
/// deserialized as `SensorReading`.
///
/// All four sit one segment below `sensor`, so `sensor.*` matches every one of
/// them and the pattern never narrows what these tests see.
///
/// | idx | record_name | value                         | valid? |
/// |-----|-------------|-------------------------------|--------|
/// | 0   | sensor.a    | `{"celsius": 21.5}`           | ✅     |
/// | 1   | sensor.b    | `"not_an_object"`             | ❌ wrong JSON type |
/// | 2   | sensor.c    | `{"celsius": 99.0}`           | ✅     |
/// | 3   | sensor.d    | `{"celsius": "not_a_number"}` | ❌ wrong value type |
fn mixed_rows() -> Vec<StoredValue> {
    vec![
        StoredValue {
            record_name: "sensor.a".into(),
            value: json!({"celsius": 21.5}),
            stored_at: 1_000,
        },
        StoredValue {
            record_name: "sensor.b".into(),
            value: json!("not_an_object"),
            stored_at: 2_000,
        },
        StoredValue {
            record_name: "sensor.c".into(),
            value: json!({"celsius": 99.0}),
            stored_at: 3_000,
        },
        StoredValue {
            record_name: "sensor.d".into(),
            value: json!({"celsius": "not_a_number"}),
            stored_at: 4_000,
        },
    ]
}

// ---------------------------------------------------------------------------
// Tests: query_latest
// ---------------------------------------------------------------------------

/// `query_latest` must return only the two valid rows and silently drop the two
/// that cannot be deserialized as `SensorReading`.
#[tokio::test]
async fn query_latest_skips_invalid_rows_and_returns_valid_ones() {
    let db = build_db(Arc::new(MockBackend { rows: mixed_rows() })).await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.*", 10)
        .await
        .expect("query_latest returned Err");

    // Two valid rows survive; two invalid rows are silently skipped.
    assert_eq!(
        results.len(),
        2,
        "expected exactly 2 valid rows, got {}: {:?}",
        results.len(),
        results
    );

    let celsius_values: Vec<f64> = results.iter().map(|r| r.celsius).collect();
    assert!(
        celsius_values.contains(&21.5),
        "missing sensor.a (celsius=21.5)"
    );
    assert!(
        celsius_values.contains(&99.0),
        "missing sensor.c (celsius=99.0)"
    );
}

/// When all rows in the backend are invalid for the target type `query_latest`
/// should return an empty `Vec` rather than an error.
#[tokio::test]
async fn query_latest_all_invalid_returns_empty_vec() {
    let all_bad = vec![
        StoredValue {
            record_name: "sensor.x".into(),
            value: json!(null),
            stored_at: 1_000,
        },
        StoredValue {
            record_name: "sensor.y".into(),
            value: json!([1, 2, 3]),
            stored_at: 2_000,
        },
        StoredValue {
            record_name: "sensor.z".into(),
            value: json!({"celsius": "oops"}),
            stored_at: 3_000,
        },
    ];

    let db = build_db(Arc::new(MockBackend { rows: all_bad })).await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.*", 10)
        .await
        .expect("query_latest returned Err");

    assert!(
        results.is_empty(),
        "expected empty Vec when all rows are invalid, got: {:?}",
        results
    );
}

/// When all backend rows are valid `query_latest` should return all of them
/// (no false-positive skipping).
#[tokio::test]
async fn query_latest_all_valid_returns_all_rows() {
    let all_good = vec![
        StoredValue {
            record_name: "sensor.a".into(),
            value: json!({"celsius": 10.0}),
            stored_at: 1_000,
        },
        StoredValue {
            record_name: "sensor.b".into(),
            value: json!({"celsius": 20.0}),
            stored_at: 2_000,
        },
    ];

    let db = build_db(Arc::new(MockBackend { rows: all_good })).await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.*", 10)
        .await
        .expect("query_latest returned Err");

    assert_eq!(
        results.len(),
        2,
        "expected 2 valid rows, got: {:?}",
        results
    );
}

// ---------------------------------------------------------------------------
// Tests: query_range
// ---------------------------------------------------------------------------

/// `query_range` shares the same `filter_map` path; invalid rows must be
/// skipped here too.
#[tokio::test]
async fn query_range_skips_invalid_rows_and_returns_valid_ones() {
    let db = build_db(Arc::new(MockBackend { rows: mixed_rows() })).await;

    // The mock ignores start/end — it always returns all rows.
    let results: Vec<SensorReading> = db
        .query_range("sensor.*", 0, u64::MAX, None)
        .await
        .expect("query_range returned Err");

    assert_eq!(
        results.len(),
        2,
        "expected exactly 2 valid rows, got {}: {:?}",
        results.len(),
        results
    );

    let celsius_values: Vec<f64> = results.iter().map(|r| r.celsius).collect();
    assert!(celsius_values.contains(&21.5), "missing sensor.a");
    assert!(celsius_values.contains(&99.0), "missing sensor.c");
}

/// `query_range` with all-invalid rows must return an empty `Vec`, not an error.
#[tokio::test]
async fn query_range_all_invalid_returns_empty_vec() {
    let all_bad = vec![StoredValue {
        record_name: "sensor.x".into(),
        value: json!("wrong"),
        stored_at: 5_000,
    }];

    let db = build_db(Arc::new(MockBackend { rows: all_bad })).await;

    let results: Vec<SensorReading> = db
        .query_range("sensor.*", 0, u64::MAX, None)
        .await
        .expect("query_range returned Err");

    assert!(results.is_empty(), "expected empty Vec, got: {:?}", results);
}

/// `query_range` with all-valid rows must return every row (no false-positive
/// skipping).
#[tokio::test]
async fn query_range_all_valid_returns_all_rows() {
    let all_good = vec![
        StoredValue {
            record_name: "sensor.a".into(),
            value: json!({"celsius": 5.5}),
            stored_at: 1_000,
        },
        StoredValue {
            record_name: "sensor.b".into(),
            value: json!({"celsius": 6.6}),
            stored_at: 2_000,
        },
        StoredValue {
            record_name: "sensor.c".into(),
            value: json!({"celsius": 7.7}),
            stored_at: 3_000,
        },
    ];

    let db = build_db(Arc::new(MockBackend { rows: all_good })).await;

    let results: Vec<SensorReading> = db
        .query_range("sensor.*", 0, u64::MAX, None)
        .await
        .expect("query_range returned Err");

    assert_eq!(
        results.len(),
        3,
        "expected 3 valid rows, got: {:?}",
        results
    );
}

// ---------------------------------------------------------------------------
// Tests: the record-pattern grammar reaches the backend
// ---------------------------------------------------------------------------

/// Rows at two depths under `sensor.`, all valid for `SensorReading`, so that
/// what a query returns is decided by the pattern alone.
fn rows_at_two_depths() -> Vec<StoredValue> {
    vec![
        StoredValue {
            record_name: "sensor.a".into(),
            value: json!({"celsius": 1.0}),
            stored_at: 1_000,
        },
        StoredValue {
            record_name: "sensor.deep.nested".into(),
            value: json!({"celsius": 2.0}),
            stored_at: 2_000,
        },
    ]
}

/// The regression #201 was made for: `*` covers *exactly one* segment, so a
/// `sensor.*` query must not reach `sensor.deep.nested`. Under the prefix-glob
/// contract it did, which is what made one pattern mean two different things
/// live and historically.
#[tokio::test]
async fn a_single_segment_wildcard_does_not_descend() {
    let db = build_db(Arc::new(MockBackend {
        rows: rows_at_two_depths(),
    }))
    .await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.*", 10)
        .await
        .expect("query_latest returned Err");

    assert_eq!(
        results,
        vec![SensorReading { celsius: 1.0 }],
        "`sensor.*` must match `sensor.a` only, never `sensor.deep.nested`"
    );
}

/// The counterpart that keeps the test above honest: `#` covers zero or more
/// segments, so the row `sensor.*` excluded is reachable — the row is in the
/// backend, and it is the pattern that decides.
#[tokio::test]
async fn a_multi_segment_wildcard_spans_every_depth() {
    let db = build_db(Arc::new(MockBackend {
        rows: rows_at_two_depths(),
    }))
    .await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.#", 10)
        .await
        .expect("query_latest returned Err");

    assert_eq!(
        results.len(),
        2,
        "`sensor.#` must span both depths, got: {:?}",
        results
    );
}

/// A key that is not dot-separated is one whole segment, so no wildcard can
/// address it — the trap the persistence docs used to lead readers into.
#[tokio::test]
async fn a_wildcard_cannot_partition_an_undotted_key() {
    let db = build_db(Arc::new(MockBackend {
        rows: vec![StoredValue {
            record_name: "sensor::legacy".into(),
            value: json!({"celsius": 3.0}),
            stored_at: 1_000,
        }],
    }))
    .await;

    let results: Vec<SensorReading> = db
        .query_latest("sensor.*", 10)
        .await
        .expect("query_latest returned Err");

    assert!(
        results.is_empty(),
        "`sensor::legacy` is a single segment; no pattern over `sensor.` reaches it"
    );
}
