//! Clients one after another on one serial line (#328).
//!
//! A UART never closes, so the server cannot see a client leave. A new
//! client's `hello` is the only sign that the peer changed: it must end the
//! previous client's subscriptions, or they keep streaming and the new
//! client's subscribe collides with them (every other reconnect got nothing).
//!
//! The pipe below stays open for the whole test, as a UART does; each client
//! speaks raw AimX frames, sending what `aimdb-client` sends: `hello` as id 1,
//! then a subscribe as id 2.

#![cfg(feature = "std")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::OneShotListener;
use aimdb_core::session::{serve, Dispatch, SessionConfig, SessionLimits};
use aimdb_core::AimDbBuilder;
use aimdb_serial_connector::connector::framed;
use aimdb_serial_connector::framing::{encode_frame, FrameAccumulator};
use aimdb_tokio_adapter::net::TokioByteStream;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temp {
    c: u64,
}

/// One AimX frame, COBS-framed for the line.
fn frame(v: Value) -> Vec<u8> {
    let mut wire = Vec::new();
    encode_frame(&serde_json::to_vec(&v).unwrap(), &mut wire);
    wire
}

/// Every frame that arrives within `window`.
async fn frames_within(rx: &mut mpsc::UnboundedReceiver<Value>, window: Duration) -> Vec<Value> {
    let mut frames = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(f)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        frames.push(f);
    }
    frames
}

#[tokio::test]
async fn every_client_on_one_serial_line_gets_its_events() {
    let (server_end, client_end) = tokio::io::duplex(64 * 1024);

    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    // The serial connector's session config: one peer for the line's life.
    let dispatch: Arc<dyn Dispatch> =
        Arc::new(AimxDispatch::new(db.clone(), AimxConfig::uds_default()));
    tokio::spawn(serve(
        OneShotListener::new(framed(TokioByteStream(server_end))),
        Arc::new(AimxCodec),
        dispatch,
        SessionConfig {
            limits: SessionLimits {
                max_connections: 1,
                max_subs_per_connection: 8,
            },
            reads_hello: false,
            acks_subscribe: false,
        },
    ));

    // The host end of the line: frames keep arriving whether or not a client
    // reads them.
    let (mut rd, mut wr) = tokio::io::split(client_end);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut acc = FrameAccumulator::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = rd.read(&mut buf).await {
            if n == 0 {
                break;
            }
            acc.push_bytes(&buf[..n]);
            while let Some(Ok(f)) = acc.next_frame() {
                if let Ok(v) = serde_json::from_slice::<Value>(&f) {
                    let _ = tx.send(v);
                }
            }
        }
    });

    let mut value = 0u64;
    for client in ["A", "B", "C"] {
        // Values produced while the previous client is gone, then whatever
        // its subscription sent: lost with it.
        for _ in 0..3 {
            value += 1;
            db.produce::<Temp>("temp", Temp { c: value }).unwrap();
        }
        frames_within(&mut rx, Duration::from_millis(100)).await;

        wr.write_all(&frame(json!({"t":"req","id":1,"method":"hello",
            "params":{"client":client,"version":aimdb_core::remote::PROTOCOL_VERSION}})))
            .await
            .unwrap();
        let welcome = frames_within(&mut rx, Duration::from_millis(100)).await;
        assert!(
            welcome.len() == 1 && welcome[0]["t"] == "reply" && welcome[0]["id"] == 1,
            "client {client}: only its Welcome after hello, got {welcome:?}"
        );

        wr.write_all(&frame(json!({"t":"sub","id":2,"topic":"temp"})))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let first = value + 1;
        for _ in 0..5 {
            value += 1;
            db.produce::<Temp>("temp", Temp { c: value }).unwrap();
        }
        let frames = frames_within(&mut rx, Duration::from_millis(300)).await;
        // A fresh subscription numbers its events from 1; the previous
        // client's would continue its own count under the same id.
        let events: Vec<(u64, u64)> = frames
            .iter()
            .filter(|f| f["t"] == "event" && f["sub"] == "2")
            .map(|f| (f["seq"].as_u64().unwrap(), f["data"]["c"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            events,
            (1..=5).zip(first..=value).collect::<Vec<_>>(),
            "client {client}: each value once, from its own subscription; got {frames:?}"
        );
        assert!(
            frames.iter().all(|f| f["t"] == "event"),
            "client {client}: no error reply to its subscribe; got {frames:?}"
        );
        // The client goes away without a word, as a closed serial port does.
    }
}
