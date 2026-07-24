//! The `QueryHandlerFn` registered by `with_persistence` produces the shared
//! `record.query` result shape — `{"records": [{topic, payload, ts}, …],
//! "total": N}`, rows sorted by `ts` ascending.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::remote::{QueryHandlerFn, QueryHandlerParams};
use aimdb_core::AimDbBuilder;
use aimdb_persistence::{
    AimDbBuilderPersistExt, BoxFuture, PersistenceBackend, PersistenceError, QueryParams,
    StoredValue,
};
use aimdb_tokio_adapter::TokioAdapter;
use serde_json::{json, Value};

/// Mock backend returning fixed rows, deliberately out of `stored_at` order to
/// prove the handler sorts.
struct MockBackend;

impl PersistenceBackend for MockBackend {
    fn store<'a>(
        &'a self,
        _record_name: &'a str,
        _value: &'a Value,
        _timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    fn query<'a>(
        &'a self,
        _record_pattern: &'a str,
        _params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        Box::pin(async {
            Ok(vec![
                StoredValue {
                    record_name: "temp.berlin".into(),
                    value: json!({"celsius": 18.0}),
                    stored_at: 2_000,
                },
                StoredValue {
                    record_name: "temp.vienna".into(),
                    value: json!({"celsius": 21.5}),
                    stored_at: 1_000,
                },
            ])
        })
    }

    fn cleanup(&self, _older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Ok(0) })
    }
}

#[tokio::test]
async fn query_handler_returns_records_total_shape() {
    let (db, runner) = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_persistence(
            Arc::new(MockBackend) as Arc<dyn PersistenceBackend>,
            Duration::from_secs(3600),
        )
        .build()
        .await
        .expect("AimDb build failed");
    tokio::spawn(runner.run());

    let handler = db
        .extensions()
        .get::<QueryHandlerFn>()
        .expect("with_persistence registers the query handler");
    let result = handler(QueryHandlerParams {
        name: "*".into(),
        limit: None,
        start: None,
        end: None,
    })
    .await
    .expect("query succeeds");

    assert_eq!(result["total"], json!(2));
    let records = result["records"].as_array().expect("records array");
    assert_eq!(records.len(), 2);
    // Sorted ascending by ts, regardless of backend order.
    assert_eq!(
        records[0],
        json!({"topic": "temp.vienna", "payload": {"celsius": 21.5}, "ts": 1_000})
    );
    assert_eq!(
        records[1],
        json!({"topic": "temp.berlin", "payload": {"celsius": 18.0}, "ts": 2_000})
    );
}
