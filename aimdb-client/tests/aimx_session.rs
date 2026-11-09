//! The engine-based [`AimxConnection`] round-trips the AimX wire — `hello`
//! handshake, RPC (`record.get`/`record.set`), a streaming subscription, and a
//! fire-and-forget write — against the **production** server (`UdsServer` →
//! `serve`/`run_session` + `AimxDispatch`) over a real Unix-domain socket,
//! standing up an actual `AimDb` and proving the wire end-to-end through the
//! shared session engine.

// Exercises the UDS transport end-to-end, so it rides that transport feature.
#![cfg(feature = "transport-uds")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_client::{AimxConnection, Stamp};
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
    assert_eq!(got.value, json!({ "level": 1 }));
    let seeded = got.stamp();

    // RPC: record.get on a missing record maps to a server error.
    assert!(conn.get_record("missing").await.is_err());

    // RPC: record.set (permission-checked) echoes the new value.
    let set = conn
        .set_record("setting", json!({ "level": 7 }))
        .await
        .expect("set setting");
    assert_eq!(set.get("value").unwrap(), &json!({ "level": 7 }));
    // AimX 4.0: the echo carries the write's stamp, the same a get returns for
    // that write, and a later write is never stamped earlier.
    let set_ts = Stamp::from_nanos(set["ts"].as_u64().expect("an integer ts"));
    let after_set = conn.get_record("setting").await.unwrap();
    assert_eq!(after_set.value, json!({ "level": 7 }));
    assert_eq!(after_set.stamp(), set_ts);
    assert!(set_ts >= seeded);

    // Streaming: a producer feeds `events`; the subscription routes updates back.
    let producer = db.producer::<Reading>("events").expect("producer");
    tokio::spawn(async move {
        for n in 1..=50 {
            producer.produce(Reading { n });
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    // Each update carries the engine's gap count alongside the value; a
    // consumer that keeps up sees no gaps.
    let mut stream = conn.subscribe("events").expect("subscribe");
    let mut last_ts = Stamp::from_nanos(0);
    for _ in 0..3 {
        let update = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("update within timeout")
            .expect("update")
            .expect("a live subscription is not a terminal error");
        assert_eq!(update.skipped, 0, "consumer keeps up, so no gap");
        assert!(update.ts >= last_ts, "one producer's stamps never decrease");
        last_ts = update.ts;
        assert!(!update.has_gap());
        let value = update.value.expect("the server sends decodable JSON");
        assert!(
            value.get("n").is_some(),
            "update carries a Reading: {value}"
        );
    }
    drop(stream);

    // Graph introspection wrappers.
    let nodes = conn.graph_nodes().await.expect("graph nodes");
    assert!(
        nodes.len() >= 2,
        "configured records should appear as nodes"
    );
    // #332: both fields are always sent, empty when there is nothing to report.
    for node in &nodes {
        assert!(node["outbound_links"].is_array(), "{node}");
        assert!(node.get("schema").is_some(), "{node}");
    }
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
    assert_eq!(after.value, json!({ "level": 9 }));
    assert!(after.stamp() >= set_ts);

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
    let mut stream = conn.subscribe("temp.#").expect("wildcard subscribe");

    // The snapshot for the seeded record arrives first, tagged with its topic,
    // and closes the late-join burst (it is the only matched record with a
    // current value) — so a consumer can tell the initial state is complete
    // without waiting for a live event.
    let snapshot = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("snapshot within timeout")
        .expect("snapshot")
        .expect("a wildcard subscribe is not refused");
    assert_eq!(snapshot.topic.as_deref(), Some("temp.vienna"));
    assert_eq!(snapshot.value, Some(json!({ "level": 1 })));
    assert!(
        snapshot.snapshot_end,
        "the burst's only snapshot must close it"
    );

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
    // Drain events until the other matched record fires. Every event must be
    // topic-tagged and inside the pattern.
    let mut saw_berlin = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some(Ok(update))) => {
                let topic = update.topic.expect("wildcard events are topic-tagged");
                assert!(
                    topic.starts_with("temp."),
                    "event from outside the pattern: {topic}"
                );
                let value = update.value.expect("the server sends decodable JSON");
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

/// The stream's next update, failing the test after two seconds.
async fn next(
    stream: &mut aimdb_core::session::BoxStream<
        'static,
        Result<aimdb_client::RecordUpdate, aimdb_client::ClientError>,
    >,
) -> aimdb_client::RecordUpdate {
    tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("an update within the timeout")
        .expect("the stream is open")
        .expect("not refused")
}

/// An exact-topic subscribe gets the record's current value as a snapshot,
/// the same one a wildcard matching only that record gets, and then only
/// later writes: the value the fresh reader starts on is not sent again.
/// A ring, or a record with no value yet, gets no snapshot (#438).
#[tokio::test]
async fn exact_subscribe_sends_the_current_value_once() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let config = AimxConfig::uds_default().socket_path(sock.to_str().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    for (key, cfg) in [
        ("temp.vienna", BufferCfg::SingleLatest),
        ("empty", BufferCfg::SingleLatest),
        ("ring", BufferCfg::SpmcRing { capacity: 16 }),
    ] {
        builder.configure::<Setting>(key, |reg| {
            reg.buffer(cfg).with_remote_access();
        });
    }
    let (db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());
    for key in ["temp.vienna", "ring"] {
        db.set_record_from_json(key, json!({ "level": 1 }))
            .expect("seed");
    }

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");
    let mut exact = conn.subscribe("temp.vienna").expect("subscribe");
    let mut wildcard = conn.subscribe("temp.#").expect("subscribe");
    let mut empty = conn.subscribe("empty").expect("subscribe");
    let mut ring = conn.subscribe("ring").expect("subscribe");
    // The server handles requests in order: once this replies, every
    // subscription above is open and its snapshots are sent.
    conn.get_record("temp.vienna").await.expect("get");

    let snapshot = next(&mut exact).await;
    assert_eq!(snapshot.value, Some(json!({ "level": 1 })));
    assert!(snapshot.snapshot_end, "the only snapshot closes the burst");
    let matched = next(&mut wildcard).await;
    assert_eq!(
        (&matched.topic, &matched.value, matched.ts),
        (&snapshot.topic, &snapshot.value, snapshot.ts),
        "exact and wildcard give the same snapshot"
    );

    for key in ["temp.vienna", "empty", "ring"] {
        db.set_record_from_json(key, json!({ "level": 2 }))
            .expect("write");
    }
    for stream in [&mut exact, &mut wildcard, &mut empty, &mut ring] {
        let update = next(stream).await;
        assert_eq!(update.value, Some(json!({ "level": 2 })), "{update:?}");
        assert!(!update.snapshot_end, "a live event: {update:?}");
    }
}

/// `record.get` is one non-destructive read (#444): it answers at once for an
/// empty record, an unknown name and a ring, and never moves the connection's
/// drain cursor, so `record.drain` still returns every unread event.
#[tokio::test]
async fn record_get_answers_at_once_and_leaves_drain_intact() {
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
    builder.configure::<Reading>("state", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");

    // Each miss answers at once (the old MCP retry took 3 s per read).
    let code = |r: Result<_, aimdb_client::ClientError>| match r {
        Err(aimdb_client::ClientError::ServerError { code, .. }) => code,
        other => panic!("expected a server error, got {other:?}"),
    };
    let started = std::time::Instant::now();
    assert_eq!(code(conn.get_record("state").await), "not_found", "empty");
    assert_eq!(
        code(conn.get_record("missing").await),
        "not_found",
        "unknown"
    );
    assert_eq!(code(conn.get_record("stream").await), "unsupported", "ring");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "three misses took {:?}",
        started.elapsed()
    );

    // Open the drain cursors, then write.
    assert_eq!(conn.drain_record("stream").await.expect("drain").count, 0);
    assert_eq!(conn.drain_record("state").await.expect("drain").count, 0);
    let ring = db.producer::<Reading>("stream").expect("producer");
    let state = db.producer::<Reading>("state").expect("producer");
    for n in 1..=3 {
        ring.produce(Reading { n });
    }
    state.produce(Reading { n: 7 });

    // Reads in between: the ring still refuses, the state answers, twice.
    assert_eq!(code(conn.get_record("stream").await), "unsupported");
    for _ in 0..2 {
        let got = conn.get_record("state").await.expect("state get");
        assert_eq!(got.value, serde_json::json!({ "n": 7 }));
    }

    // Drain still sees every event the gets did not consume.
    let mut drained = Vec::new();
    for _ in 0..50 {
        drained.extend(conn.drain_record("stream").await.expect("drain").values);
        if drained.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ns: Vec<_> = drained.iter().map(|v| v.value["n"].clone()).collect();
    assert_eq!(ns, [1, 2, 3]);
    let state_drain = conn.drain_record("state").await.expect("drain");
    let ns: Vec<_> = state_drain
        .values
        .iter()
        .map(|v| v.value["n"].clone())
        .collect();
    assert_eq!(ns, [7]);

    drop(conn);
}

/// A refused subscription reaches the caller as one terminal `Err` item, not as
/// a silent end of stream.
///
/// A consumer that cannot tell "denied" from "the record closed" either
/// re-subscribes forever or reports a clean session for a subscription that
/// never started. `max_subs_per_connection(0)` makes the server refuse the very
/// first subscribe deterministically.
#[tokio::test]
async fn a_refused_subscription_is_a_terminal_error_not_a_silent_eof() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let config = AimxConfig::uds_default()
        .socket_path(sock.to_str().unwrap())
        .max_subs_per_connection(0);
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    builder.configure::<Setting>("denied", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (_db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");

    // `subscribe` itself succeeds — the refusal rides the stream, since the
    // server answers a subscribe only on failure.
    let mut stream = conn.subscribe("denied").expect("subscribe is dispatched");

    let item = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("the refusal must arrive, not hang")
        .expect("a refused subscription yields an item, not an empty stream");
    let err = item.expect_err("the refusal is an Err item");
    assert!(
        matches!(&err, aimdb_client::ClientError::ServerError { code, .. } if code == "denied"),
        "unexpected error for a refused subscription: {err}"
    );

    // ...and the stream ends after it.
    let after = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("the stream must end promptly after the refusal");
    assert!(after.is_none(), "the terminal error is the last item");

    drop(conn);
}

/// `watch::arm` then one write: `await_updates` sees exactly that write, for
/// each buffer type, on a fresh connection. A SingleLatest subscriber starts
/// warm, so its seeded value must be held back by the baseline.
#[tokio::test]
async fn armed_watch_sees_only_the_write_after_it() {
    use aimdb_client::watch::{arm, await_updates, WatchStatus};

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let mut policy = SecurityPolicy::read_write();
    for key in ["latest", "ring", "mailbox"] {
        policy.allow_write_key(key);
    }
    let config = AimxConfig::uds_default()
        .socket_path(sock.to_str().unwrap())
        .security_policy(policy);
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    for (key, cfg) in [
        ("latest", BufferCfg::SingleLatest),
        ("ring", BufferCfg::SpmcRing { capacity: 16 }),
        ("mailbox", BufferCfg::Mailbox),
    ] {
        builder.configure::<Setting>(key, |reg| {
            reg.buffer(cfg).with_remote_access();
        });
    }
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    for key in ["latest", "ring", "mailbox"] {
        // Seeded before arming: it must never count as the response.
        db.set_record_from_json(key, json!({ "level": 1 }))
            .expect("seed");

        let conn = AimxConnection::connect(sock.to_str().unwrap())
            .await
            .expect("connect");
        let armed = arm(&conn, key).await.expect("arm");
        if key == "latest" {
            assert!(
                armed.baseline.is_some(),
                "a seeded SingleLatest has a stamp"
            );
        }

        db.set_record_from_json(key, json!({ "level": 2 }))
            .expect("write after arm");

        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let outcome = await_updates(armed.stream, armed.baseline, 1, Some(deadline), |u| {
            seen.push(u.value.clone());
            Ok(())
        })
        .await
        .expect("no refusal");
        assert_eq!(outcome.outcome, WatchStatus::Ok, "{key}: {outcome:?}");
        assert_eq!(seen, [Some(json!({ "level": 2 }))], "{key}");
    }
}

/// Arming on an unknown record is refused up front, not reported as a wait.
#[tokio::test]
async fn arm_on_an_unknown_record_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    let config = AimxConfig::uds_default().socket_path(sock.to_str().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    builder.configure::<Setting>("known", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (_db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());

    let conn = AimxConnection::connect(sock.to_str().unwrap())
        .await
        .expect("connect");
    assert!(aimdb_client::watch::arm(&conn, "missing").await.is_err());
}
