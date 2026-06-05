//! End-to-end AimX over the tokio serial transport, without hardware: the two
//! ends of a `tokio::io::duplex()` pipe stand in for a crossover serial cable.
//! The production server (`serve` + `AimxDispatch`) answers on one end; the
//! `run_client` engine drives RPC on the other — proving `TokioSerialConnection`'s
//! COBS framing carries the real protocol both directions.

#![cfg(feature = "_test-tokio")]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    run_client, serve, BoxFut, ClientConfig, Connection, Dialer, Dispatch, Listener, Payload,
    SessionConfig, SessionLimits, TransportError, TransportResult,
};
use aimdb_core::AimDbBuilder;
use aimdb_serial_connector::tokio_transport::TokioSerialConnection;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::DuplexStream;

/// A writable config-style record (SingleLatest, no producer → remotely settable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Setting {
    level: u64,
}

/// One-shot dialer over an in-memory duplex end (stands in for opening the port).
struct OnceDialer(Mutex<Option<DuplexStream>>);

impl Dialer for OnceDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let end = self.0.lock().unwrap().take().ok_or(TransportError::Io)?;
            Ok(Box::new(TokioSerialConnection::new(end)) as Box<dyn Connection>)
        })
    }
}

/// One-shot listener over the other duplex end (point-to-point, like a UART).
struct OnceListener(Option<DuplexStream>);

impl Listener for OnceListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            match self.0.take() {
                Some(end) => Ok(Box::new(TokioSerialConnection::new(end)) as Box<dyn Connection>),
                None => core::future::pending().await,
            }
        })
    }
}

#[tokio::test]
async fn aimx_roundtrips_over_the_serial_transport() {
    let (server_end, client_end) = tokio::io::duplex(8192);

    // A real server db with one remotely-readable record (no connector — we drive
    // `serve` directly over the duplex below).
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());
    db.set_record_from_json("setting", json!({ "level": 42 }))
        .expect("seed setting");

    // Server: AimX dispatch over the server end of the pipe.
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
        OnceListener(Some(server_end)),
        Arc::new(AimxCodec),
        dispatch,
        session_config,
    ));

    // Client: the proactive engine over the client end.
    let client_config = ClientConfig {
        sends_hello: false,
        ..ClientConfig::default()
    };
    let (handle, engine) = run_client(
        OnceDialer(Mutex::new(Some(client_end))),
        AimxCodec,
        client_config,
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);

    // RPC: record.get {name:"setting"} → the seeded value, carried over COBS frames.
    let params: Payload = serde_json::to_vec(&json!({ "name": "setting" }))
        .unwrap()
        .into();
    let reply = tokio::time::timeout(Duration::from_secs(5), handle.call("record.get", params))
        .await
        .expect("rpc within timeout")
        .expect("rpc ok");
    let value: serde_json::Value = serde_json::from_slice(&reply).expect("json reply");
    assert_eq!(value, json!({ "level": 42 }));

    // A second RPC reuses the same framed connection (proves the stream re-syncs
    // frame-to-frame, not just on the first message).
    let list = tokio::time::timeout(
        Duration::from_secs(5),
        handle.call("record.list", Payload::from(&b"{}"[..])),
    )
    .await
    .expect("rpc within timeout")
    .expect("rpc ok");
    let records: serde_json::Value = serde_json::from_slice(&list).expect("json reply");
    assert!(
        records.as_array().is_some_and(|a| !a.is_empty()),
        "record.list returns the configured records: {records}"
    );
}
