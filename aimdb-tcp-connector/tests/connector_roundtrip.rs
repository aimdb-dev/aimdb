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
use aimdb_core::AimDbBuilder;
use aimdb_tcp_connector::connector::{TcpClient, TcpServer};
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
