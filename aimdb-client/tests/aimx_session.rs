//! The engine-based [`AimxConnection`] round-trips the AimX-v2 wire — `hello`
//! handshake, RPC (`record.get`/`record.set`), a streaming subscription, and a
//! fire-and-forget write — against the **production** server (`UdsServer` →
//! `serve`/`run_session` + `AimxDispatch`) over a real Unix-domain socket,
//! standing up an actual `AimDb` and proving the wire end-to-end through the
//! shared session engine.

// Exercises the UDS transport end-to-end, so it rides that transport feature.
#![cfg(feature = "transport-uds")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_client::AimxConnection;
use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A writable config-style record (SingleLatest, no producer → remotely settable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Setting {
    level: u64,
}

/// A streamed record (SpmcRing) fed by a producer in the test.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reading {
    n: u64,
}

#[tokio::test]
async fn aimx_roundtrip_over_uds_production_server() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    // ReadWrite policy with `setting` writable; `events` stays read-only.
    let mut policy = SecurityPolicy::read_write();
    policy.allow_write_key("setting");
    let config = AimxConfig::uds_default()
        .socket_path(sock.to_str().unwrap())
        .security_policy(policy)
        .max_connections(8)
        .max_subs_per_connection(8);

    // Build a real AimDb with two remote-accessible records, served over UDS via
    // the production `UdsServer` connector (binds during `build()`).
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    builder.configure::<Reading>("events", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
            .with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    // The runner drives both the records and the UDS serve loop (spawn-free).
    tokio::spawn(runner.run());

    // Seed the writable record before connecting so `record.get` has a value.
    db.set_record_from_json("setting", json!({ "level": 1 }))
        .expect("seed setting");

    // Connect: performs the `hello` handshake and captures the Welcome.
    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");
    assert_eq!(conn.server_info().server, "aimdb");
    assert!(conn
        .server_info()
        .permissions
        .contains(&"write".to_string()));
    assert!(conn
        .server_info()
        .writable_records
        .contains(&"setting".to_string()));

    // RPC: record.get on the seeded record.
    let got = conn.get_record("setting").await.expect("get setting");
    assert_eq!(got, json!({ "level": 1 }));

    // RPC: record.get on a missing record maps to a server error.
    assert!(conn.get_record("missing").await.is_err());

    // RPC: record.set (permission-checked) echoes the new value.
    let set = conn
        .set_record("setting", json!({ "level": 7 }))
        .await
        .expect("set setting");
    assert_eq!(set.get("value").unwrap(), &json!({ "level": 7 }));
    assert_eq!(
        conn.get_record("setting").await.unwrap(),
        json!({ "level": 7 })
    );

    // Streaming: a producer feeds `events`; the subscription routes updates back.
    let producer = db.producer::<Reading>("events").expect("producer");
    tokio::spawn(async move {
        for n in 1..=50 {
            producer.produce(Reading { n });
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let mut stream = conn.subscribe("events").expect("subscribe");
    for _ in 0..3 {
        let ev = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("event within timeout")
            .expect("event");
        assert!(ev.get("n").is_some(), "event carries a Reading: {ev}");
    }

    // Graph introspection wrappers.
    let nodes = conn.graph_nodes().await.expect("graph nodes");
    assert!(
        nodes.len() >= 2,
        "configured records should appear as nodes"
    );
    let _edges = conn.graph_edges().await.expect("graph edges");
    let topo = conn.graph_topo_order().await.expect("topo order");
    assert!(!topo.is_empty(), "topo order should list the records");

    // Drain wrapper: cold-start creates the per-connection cursor; the response
    // echoes the record name.
    let drained = conn.drain_record("events").await.expect("drain events");
    assert_eq!(drained.record_name, "events");

    // Fire-and-forget write, then a follow-up RPC. FIFO over the single
    // connection guarantees the write is processed before the reply returns.
    conn.write_record("setting", json!({ "level": 9 }))
        .expect("write");
    let after = conn.get_record("setting").await.expect("get after write");
    assert_eq!(after, json!({ "level": 9 }));

    drop(conn); // stops the client engine
}

/// One wildcard subscription fans in every matching record: events arrive
/// tagged with the concrete record topic, and matched records with a current
/// value are delivered up front as snapshots.
#[tokio::test]
async fn wildcard_subscribe_fans_in_matching_records() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let config = AimxConfig::uds_default().socket_path(sock.to_str().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    for key in ["temp.vienna", "temp.berlin", "humidity.london"] {
        builder.configure::<Setting>(key, |reg| {
            reg.buffer(BufferCfg::SingleLatest).with_remote_access();
        });
    }
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    // Seed one matched record before subscribing — it must arrive as a
    // late-join snapshot on the wildcard stream.
    db.set_record_from_json("temp.vienna", json!({ "level": 1 }))
        .expect("seed vienna");

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");
    let mut stream = conn
        .subscribe_with_topics("temp.#")
        .expect("wildcard subscribe");

    // The snapshot for the seeded record arrives first, tagged with its topic.
    let (topic, value) = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("snapshot within timeout")
        .expect("snapshot");
    assert_eq!(topic.as_deref(), Some("temp.vienna"));
    assert_eq!(value, json!({ "level": 1 }));

    // Live updates from both matched records ride the one subscription, each
    // tagged; the non-matching record must never appear.
    let db2 = db.clone();
    tokio::spawn(async move {
        for n in 1..=50u64 {
            let _ = db2.set_record_from_json("temp.berlin", json!({ "level": n }));
            let _ = db2.set_record_from_json("humidity.london", json!({ "level": n }));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    // Drain events until the other matched record fires (the seeded record's
    // current value may replay first — SingleLatest readers start warm). Every
    // event must be topic-tagged and inside the pattern.
    let mut saw_berlin = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some((topic, value))) => {
                let topic = topic.expect("wildcard events are topic-tagged");
                assert!(
                    topic.starts_with("temp."),
                    "event from outside the pattern: {topic}"
                );
                assert!(value.get("level").is_some());
                if topic == "temp.berlin" {
                    saw_berlin = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        saw_berlin,
        "the wildcard stream must carry the other matched record's updates"
    );

    drop(conn);
}

/// `record.get` on a ring (`SpmcRing`) record has no canonical latest, so it
/// falls back to draining the connection's cursor for the most-recent value.
#[tokio::test]
async fn record_get_on_ring_falls_back_to_drain() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let config = AimxConfig::uds_default().socket_path(sock.to_str().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    builder.configure::<Reading>("stream", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");
    let producer = db.producer::<Reading>("stream").expect("producer");

    // First get opens the cursor; a fresh broadcast reader starts at the tail, so
    // it sees nothing until a value is produced afterwards.
    assert!(conn.get_record("stream").await.is_err());

    // Produce after the cursor is open; get now returns the most-recent value.
    let mut got = None;
    for n in 1..=50u64 {
        producer.produce(Reading { n });
        tokio::time::sleep(Duration::from_millis(10)).await;
        if let Ok(v) = conn.get_record("stream").await {
            got = Some(v);
            break;
        }
    }
    let got = got.expect("ring get returns a value after producing");
    assert!(got.get("n").is_some(), "ring get yields a Reading: {got}");

    drop(conn);
}
