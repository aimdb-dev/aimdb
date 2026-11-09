//! `get_record` answers at once and leaves `drain_record` intact (#444).
//!
//! It used to retry `not_found` for up to 3 s, so an empty record or a bad
//! name cost the whole window, and a ring was read through the drain cursor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::AimDbBuilder;
use aimdb_mcp::tools;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    n: u64,
}

fn args(endpoint: &str, record: &str) -> Option<Value> {
    Some(json!({ "endpoint": endpoint, "record_name": record }))
}

#[tokio::test]
async fn get_record_answers_at_once_and_leaves_drain_intact() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");
    let endpoint = sock.to_str().unwrap().to_string();

    let config = AimxConfig::uds_default().socket_path(&endpoint);
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    builder.configure::<Reading>("stream", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    builder.configure::<Reading>("state", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());
    tools::init_connection_pool(aimdb_mcp::connection::ConnectionPool::new());

    // Each miss used to take ~3 s. Allow 1 s for all three together.
    let started = Instant::now();
    for (record, code) in [
        ("state", "not_found"),    // single_latest, no value yet (the issue)
        ("missing", "not_found"),  // unknown name
        ("stream", "unsupported"), // a ring has no current value
    ] {
        let err = tools::get_record(args(&endpoint, record))
            .await
            .expect_err(record)
            .to_string();
        assert!(err.contains(code), "{record}: {err}");
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "three misses took {:?}",
        started.elapsed()
    );

    // Open the ring's drain cursor, write, and read the ring with get in
    // between: drain still returns every event.
    let drain = || {
        tools::drain_record(Some(
            json!({ "endpoint": endpoint, "record_name": "stream" }),
        ))
    };
    assert_eq!(drain().await.expect("drain")["count"], 0);
    let producer = db.producer::<Reading>("stream").expect("producer");
    for n in 1..=3 {
        producer.produce(Reading { n });
    }
    assert!(tools::get_record(args(&endpoint, "stream")).await.is_err());

    let mut ns = Vec::new();
    for _ in 0..50 {
        let page = drain().await.expect("drain");
        ns.extend(
            page["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["value"]["n"].clone()),
        );
        if ns.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(ns, [1, 2, 3]);

    // A state record with a value answers with it, at once.
    db.producer::<Reading>("state")
        .unwrap()
        .produce(Reading { n: 7 });
    let got = tools::get_record(args(&endpoint, "state"))
        .await
        .expect("get");
    assert_eq!(got["value"], json!({ "n": 7 }));
}
