//! Phase 3 **server-port** exit criterion: the engine-based [`AimxConnection`]
//! round-trips the reshaped AimX-v2 wire — `hello` handshake, RPC
//! (`record.get`/`record.set`), a streaming subscription, and a
//! fire-and-forget write — against the **production** server
//! ([`build_aimx_server`] → `serve`/`run_session` + `AimxDispatch`) over a real
//! Unix-domain socket.
//!
//! This swaps the client-half milestone's test-local `UdsListener` +
//! `TestDispatch` for real server code standing up an actual `AimDb`, proving
//! the reshaped wire end-to-end through the shared session engine.

use std::sync::Arc;
use std::time::Duration;

use aimdb_client::AimxConnection;
use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::build_aimx_server;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
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

    // Build a real AimDb with two remote-accessible records.
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    builder.configure::<Reading>("events", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
            .with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    // Seed the writable record before connecting so `record.get` has a value.
    db.set_record_from_json("setting", json!({ "level": 1 }))
        .expect("seed setting");

    // ReadWrite policy with `setting` writable; `events` stays read-only.
    let mut policy = SecurityPolicy::read_write();
    policy.allow_write_key("setting");
    let config = AimxConfig::uds_default()
        .socket_path(&sock)
        .security_policy(policy)
        .max_connections(8)
        .max_subs_per_connection(8);

    // Production server future, driven on a task (the engine itself is spawn-free).
    let server = tokio::spawn(build_aimx_server(db.clone(), config).expect("bind server"));

    // Connect: performs the `hello` handshake and captures the Welcome.
    let conn = AimxConnection::connect(&sock).await.expect("connect");
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
    server.abort();
}
