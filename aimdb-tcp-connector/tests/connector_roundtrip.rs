//! `TcpClient`/`TcpServer` over the adapter's stream transports, end to end
//! through a real `AimDb`.
//!
//! The socket comes from `TokioNet`; this crate supplies only the length-prefix
//! framer. Complements `tokio_roundtrip.rs`, which drives the same framed
//! transports straight through the session engines: that file covers the wire
//! path, this one covers the connector builders sitting on top of it.
#![cfg(feature = "_test-tokio")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{run_client, ClientConfig, Payload};
use aimdb_core::AimDbBuilder;
use aimdb_tcp_connector::connector::{framed_dialer, TcpClient, TcpServer};
use aimdb_tokio_adapter::net::TokioNet;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Setting {
    level: u64,
}

async fn db() -> Arc<aimdb_core::AimDb> {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, _runner) = builder.build().await.expect("build db");
    Arc::new(db)
}

/// The server accepts on an adapter listener and serves AimX over it.
#[tokio::test]
async fn server_serves_over_an_adapter_listener() {
    let listener = TokioNet::listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("bound addr");

    let db = db().await;
    let server = TcpServer::new(listener);
    let futures = server.build(&db).await.expect("build server");
    assert_eq!(futures.len(), 1, "one serve future");
    let serving = tokio::spawn(async move {
        for f in futures {
            f.await;
        }
    });

    // A bare TCP connect proves the listener is live and accepting.
    let peer = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect timed out")
        .expect("connect");
    assert!(peer.peer_addr().is_ok());

    serving.abort();
}

/// The client registers under the TCP scheme and builds its pump futures.
#[tokio::test]
async fn client_builds_over_an_adapter_dialer() {
    let listener = TokioNet::listen("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("bound addr").port();

    let db = db().await;
    let client = TcpClient::new(TokioNet::tcp(), "127.0.0.1", port);
    assert_eq!(ConnectorBuilder::scheme(&client), "tcp");

    let futures = client.build(&db).await.expect("build client");
    assert!(
        !futures.is_empty(),
        "client contributes at least one future"
    );
}

/// The listener is moved in, so a second `build` is refused rather than
/// silently serving nothing.
#[tokio::test]
async fn a_second_build_is_refused() {
    let listener = TokioNet::listen("127.0.0.1:0").await.expect("bind");
    let db = db().await;
    let server = TcpServer::new(listener);

    server.build(&db).await.expect("first build");
    let Err(err) = server.build(&db).await else {
        panic!("a second build must fail");
    };
    assert!(
        format!("{err}").contains("already taken"),
        "unexpected error: {err}"
    );
}

/// A `build()` future dropped before it is polled must not consume the
/// listener — otherwise a lost `select!` arm or an unrelated builder error
/// leaves the server permanently unbuildable.
#[tokio::test]
async fn an_unpolled_build_leaves_the_listener_in_place() {
    let listener = TokioNet::listen("127.0.0.1:0").await.expect("bind");
    let db = db().await;
    let server = TcpServer::new(listener);

    drop(server.build(&db));

    let futures = server
        .build(&db)
        .await
        .expect("the listener must survive an unpolled build");
    assert_eq!(futures.len(), 1);
}

/// The builder's own wiring, which neither existing suite observes.
///
/// `tokio_roundtrip.rs` round-trips AimX but hand-builds its `SessionConfig`
/// and dispatch; the tests above drive `TcpServer::build` but only check that a
/// socket accepts. Between them sits everything `build` actually *does*, and two
/// distinct pieces of it are asserted here — both verified by mutation, because
/// a test that cannot fail is worse than none:
///
/// - **the config reaches the dispatch**: dropping it in `with_config` costs the
///   security policy and the write comes back `Denied`.
/// - **`apply_writable` runs**: it has exactly one caller and marks record
///   storage writable from that policy. It does *not* gate writes — the policy
///   does, in `ensure_writable` — so it is only visible in the metadata
///   `record.list` returns, which is what the last assertion reads.
#[tokio::test]
async fn a_policy_allowed_write_lands_through_the_built_server() {
    let listener = TokioNet::listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("bound addr");

    let mut policy = SecurityPolicy::read_write();
    policy.allow_write_key("setting");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            TcpServer::new(listener).with_config(AimxConfig::uds_default().security_policy(policy)),
        );
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    db.set_record_from_json("setting", serde_json::json!({ "level": 1 }))
        .expect("seed setting");
    tokio::spawn(runner.run());

    let (handle, engine) = run_client(
        framed_dialer(TokioNet::tcp(), addr.ip().to_string(), addr.port()),
        AimxCodec,
        ClientConfig {
            sends_hello: false,
            ..ClientConfig::default()
        },
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);

    let set: Payload = serde_json::to_vec(&serde_json::json!({
        "name": "setting",
        "value": { "level": 7 }
    }))
    .unwrap()
    .into();
    tokio::time::timeout(Duration::from_secs(5), handle.call("record.set", set))
        .await
        .expect("record.set within timeout")
        .expect("the policy marks 'setting' writable, so the write must be allowed");

    // Read back over the wire: an ack alone would not prove the value landed.
    let get: Payload = serde_json::to_vec(&serde_json::json!({ "name": "setting" }))
        .unwrap()
        .into();
    let reply = tokio::time::timeout(Duration::from_secs(5), handle.call("record.get", get))
        .await
        .expect("record.get within timeout")
        .expect("record.get ok");
    let value: serde_json::Value = serde_json::from_slice(&reply).expect("json reply");
    assert_eq!(value, serde_json::json!({ "level": 7 }));

    // `apply_writable` marks storage from the policy, and `record.list` is the
    // only place that marking surfaces. Without it a client cannot tell which
    // records it may write.
    let list = tokio::time::timeout(
        Duration::from_secs(5),
        handle.call("record.list", Payload::from(&b"{}"[..])),
    )
    .await
    .expect("record.list within timeout")
    .expect("record.list ok");
    let records: serde_json::Value = serde_json::from_slice(&list).expect("json reply");
    let setting = records
        .as_array()
        .expect("record.list returns an array")
        .iter()
        .find(|r| r["record_key"] == "setting")
        .expect("'setting' is listed");
    assert_eq!(
        setting["writable"],
        serde_json::json!(true),
        "the policy's writable marking must reach record metadata"
    );
}
