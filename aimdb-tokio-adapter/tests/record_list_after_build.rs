//! Regression tests for `list_records()` metadata after `build()` (issue #470).
//!
//! `build()` extracts registered producer, transform, and consumer closures out
//! of `TypedRecord` so `AimDbRunner` can drive them. Introspection queries that
//! run after `build()` — `AimDb::list_records`, `TypedRecord::record_origin`,
//! `has_producer`, `has_transform`, `consumer_count`, and the no-producer-override
//! guard on `set_record_from_json` — must still observe the configured topology
//! and agree with `DependencyGraph::nodes`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::graph::RecordOrigin;
use aimdb_core::{AimDb, AimDbBuilder, DbError, DbResult};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde_json::json;

type TaskFutures = Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>;

struct TestConnector;

impl ConnectorBuilder for TestConnector {
    fn build<'a>(
        &'a self,
        _db: &'a AimDb,
    ) -> Pin<Box<dyn Future<Output = DbResult<TaskFutures>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn scheme(&self) -> &str {
        "test"
    }
}

fn find_record<'a>(
    records: &'a [aimdb_core::remote::RecordMetadata],
    key: &str,
) -> &'a aimdb_core::remote::RecordMetadata {
    records
        .iter()
        .find(|r| r.record_key == key)
        .unwrap_or_else(|| panic!("missing record '{key}' in list_records()"))
}

#[tokio::test]
async fn list_records_after_build_preserves_origin_and_counts() {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(TestConnector);

    builder.configure::<u32>("src", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .tap(|_, _| async {})
            .tap(|_, _| async {})
            .source(|_, _| async {});
    });

    builder.configure::<u32>("derived", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .tap(|_, _| async {})
            .transform::<u32, _>("src", |b| b.map(|v: &u32| Some(*v)));
    });

    builder.configure::<u32>("joined", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .tap(|_, _| async {})
            .transform_join(|b| {
                b.input::<u32>("src").input::<u32>("derived").on_triggers(
                    |mut rx, _producer| async move { while rx.recv().await.is_ok() {} },
                )
            });
    });

    builder.configure::<u32>("inbound", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .tap(|_, _| async {})
            .link_from("test://sensor/in")
            .with_deserializer(|_ctx, _bytes| Ok(0u32))
            .finish();
    });

    builder.configure::<u32>("passive_tapped", |r| {
        r.buffer(BufferCfg::SingleLatest).tap(|_, _| async {});
    });

    builder.configure::<u32>("passive_bare", |r| {
        r.buffer(BufferCfg::SingleLatest);
    });

    let (db, _runner) = builder.build().await.unwrap();
    let records = db.list_records();

    let src = find_record(&records, "src");
    assert_eq!(src.origin, RecordOrigin::Source);
    assert_eq!(src.producer_count, 1);
    assert_eq!(src.consumer_count, 2);

    let derived = find_record(&records, "derived");
    assert_eq!(
        derived.origin,
        RecordOrigin::Transform {
            input: "src".to_string()
        }
    );
    assert_eq!(derived.producer_count, 0);
    assert_eq!(derived.consumer_count, 1);

    let joined = find_record(&records, "joined");
    assert_eq!(
        joined.origin,
        RecordOrigin::TransformJoin {
            inputs: vec!["src".to_string(), "derived".to_string()]
        }
    );
    assert_eq!(joined.producer_count, 0);
    assert_eq!(joined.consumer_count, 1);

    let inbound = find_record(&records, "inbound");
    assert_eq!(
        inbound.origin,
        RecordOrigin::Link {
            protocol: "test".to_string(),
            address: "test://sensor/in".to_string(),
        }
    );
    assert_eq!(inbound.producer_count, 0);
    assert_eq!(inbound.consumer_count, 1);

    let passive_tapped = find_record(&records, "passive_tapped");
    assert_eq!(passive_tapped.origin, RecordOrigin::Passive);
    assert_eq!(passive_tapped.producer_count, 0);
    assert_eq!(passive_tapped.consumer_count, 1);

    let passive_bare = find_record(&records, "passive_bare");
    assert_eq!(passive_bare.origin, RecordOrigin::Passive);
    assert_eq!(passive_bare.producer_count, 0);
    assert_eq!(passive_bare.consumer_count, 0);

    // Every record reported by list_records() must agree with the dependency graph.
    let graph = db.inner().dependency_graph();
    assert_eq!(records.len(), graph.nodes.len());
    for node in &graph.nodes {
        let meta = find_record(&records, &node.key);
        assert_eq!(
            meta.origin, node.origin,
            "origin mismatch between list_records() and graph.nodes for '{}'",
            node.key
        );
        assert_eq!(
            meta.consumer_count, node.tap_count,
            "consumer_count vs tap_count mismatch for '{}'",
            node.key
        );
    }
}

#[tokio::test]
async fn remote_write_still_rejects_source_and_transform_records_after_build() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter::new().unwrap()));

    builder.configure::<u32>("src", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .source(|_, _| async {});
    });

    builder.configure::<u32>("derived", |r| {
        r.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .transform::<u32, _>("src", |b| b.map(|v: &u32| Some(*v)));
    });

    builder.configure::<u32>("cfg", |r| {
        r.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    let (db, _runner) = builder.build().await.unwrap();

    let src_err = db.set_record_from_json("src", json!(42)).unwrap_err();
    assert!(
        matches!(src_err, DbError::PermissionDenied { .. }),
        "expected PermissionDenied for source record after build(), got {src_err:?}"
    );

    let derived_err = db.set_record_from_json("derived", json!(42)).unwrap_err();
    assert!(
        matches!(derived_err, DbError::PermissionDenied { .. }),
        "expected PermissionDenied for transform record after build(), got {derived_err:?}"
    );

    db.set_record_from_json("cfg", json!(42)).unwrap();
    assert_eq!(db.latest::<u32>("cfg").unwrap().unwrap().value, 42);
}
