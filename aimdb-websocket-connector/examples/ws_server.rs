//! Minimal runnable WebSocket **server** demo (doc 039-validation Layer 5) — the
//! first real consumer of the connector and a manual-smoke vehicle.
//!
//! Run:
//! ```text
//! cargo run -p aimdb-websocket-connector --example ws_server
//! ```
//! Then connect a client and subscribe to the ticking `counter` record:
//! ```text
//! wscat -c ws://127.0.0.1:8080/ws
//! > {"type":"subscribe","topics":["counter"]}
//! < {"type":"subscribed","topics":["counter"]}
//! < {"type":"data","topic":"counter","payload":{"n":1},"ts":...}
//! ```
//! Or write to the inbound `echo` record:
//! ```text
//! > {"type":"write","topic":"echo","payload":{"msg":"hi"}}
//! ```

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::WebSocketConnector;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Counter {
    n: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Echo {
    msg: String,
}

#[tokio::main]
async fn main() {
    let addr = "127.0.0.1:8080";

    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            WebSocketConnector::new()
                .bind(addr)
                .path("/ws")
                .with_late_join(true),
        );

    // Outbound: pushed to every subscribed client.
    sb.configure::<Counter>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws://counter")
            .with_serializer(|_ctx, c: &Counter| Ok(serde_json::to_vec(c).unwrap()))
            .finish();
    });
    // Inbound: clients may `write` to it.
    sb.configure::<Echo>("echo", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws://echo")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Echo>(d).map_err(|e| e.to_string())
            })
            .finish();
    });

    let (db, runner) = sb.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    println!("WS server listening on ws://{addr}/ws");
    println!("  subscribe:  wscat -c ws://{addr}/ws  →  {{\"type\":\"subscribe\",\"topics\":[\"counter\"]}}");

    let mut n = 0u64;
    loop {
        n += 1;
        db.set_record_from_json("counter", json!({ "n": n }))
            .expect("set counter");
        if let Some(echo) = db.try_latest_as_json("echo") {
            println!("echo record = {echo}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
