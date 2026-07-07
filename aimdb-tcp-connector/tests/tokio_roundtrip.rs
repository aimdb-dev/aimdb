#![cfg(feature = "_test-tokio")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    run_client, serve, ClientConfig, Dispatch, Payload, SessionConfig, SessionLimits,
};
use aimdb_core::AimDbBuilder;
use aimdb_tcp_connector::tokio_transport::{TcpDialer, TcpListener};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Setting {
    level: u64,
}

#[tokio::test]
async fn aimx_roundtrips_over_tcp_loopback() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());
    db.set_record_from_json("setting", json!({ "level": 42 }))
        .expect("seed setting");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp");
    let addr = listener.local_addr().expect("local addr");

    let dispatch: Arc<dyn Dispatch> =
        Arc::new(AimxDispatch::new(db.clone(), AimxConfig::uds_default()));
    let session_config = SessionConfig {
        limits: SessionLimits {
            max_connections: 1,
            max_subs_per_connection: 8,
        },
        reads_hello: false,
        acks_subscribe: false,
    };
    tokio::spawn(serve(
        TcpListener::new(listener),
        Arc::new(AimxCodec),
        dispatch,
        session_config,
    ));

    let client_config = ClientConfig {
        sends_hello: false,
        ..ClientConfig::default()
    };
    let (handle, engine) = run_client(
        TcpDialer::new(addr.to_string()),
        AimxCodec,
        client_config,
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);

    let params: Payload = serde_json::to_vec(&json!({ "name": "setting" }))
        .unwrap()
        .into();
    let reply = tokio::time::timeout(Duration::from_secs(5), handle.call("record.get", params))
        .await
        .expect("rpc within timeout")
        .expect("rpc ok");
    let value: serde_json::Value = serde_json::from_slice(&reply).expect("json reply");
    assert_eq!(value, json!({ "level": 42 }));

    let list = tokio::time::timeout(
        Duration::from_secs(5),
        handle.call("record.list", Payload::from(&b"{}"[..])),
    )
    .await
    .expect("rpc within timeout")
    .expect("rpc ok");
    let records: serde_json::Value = serde_json::from_slice(&list).expect("json reply");
    assert!(records.as_array().is_some_and(|a| !a.is_empty()));
}
