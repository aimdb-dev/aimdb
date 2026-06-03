//! Minimal runnable WebSocket **client** demo — pairs with the `ws_server` example
//! to show AimDB ↔ AimDB mirroring over a real socket.
//!
//! Two terminals:
//! ```text
//! cargo run -p aimdb-websocket-connector --example ws_server
//! cargo run -p aimdb-websocket-connector --example ws_client --features client
//! ```
//!
//! The client dials the server at `ws://127.0.0.1:8080/ws` and:
//! - mirrors the server's ticking `counter` into a local record (printing each
//!   update) — `link_from("ws-client://counter")` ← the server's `link_to("ws://counter")`;
//! - writes an `echo` record back to the server — `link_to("ws-client://echo")`
//!   → the server's `link_from("ws://echo")`, which the server prints.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::WsClientConnector;
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
    let url = "ws://127.0.0.1:8080/ws";
    println!("ws_client: dialing {url} (start `--example ws_server` first)");

    let mut cb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(WsClientConnector::new(url));

    // Subscribe to the server's `counter` and mirror it into a local record.
    cb.configure::<Counter>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws-client://counter")
            .with_deserializer_raw(|d: &[u8]| {
                serde_json::from_slice::<Counter>(d).map_err(|e| e.to_string())
            })
            .finish();
    });

    // Produce an `echo` locally; the client connector writes it to the server.
    cb.configure::<Echo>("echo", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws-client://echo")
            .with_serializer_raw(|e: &Echo| Ok(serde_json::to_vec(e).unwrap()))
            .finish();
    });

    let (db, runner) = cb.build().await.expect("build client db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    // Each tick: write an echo (the server prints it) and print the mirrored
    // counter whenever it advances.
    let mut last: Option<serde_json::Value> = None;
    let mut tick = 0u64;
    loop {
        tick += 1;
        db.set_record_from_json(
            "echo",
            json!({ "msg": format!("hello #{tick} from ws_client") }),
        )
        .ok();

        if let Some(v) = db.try_latest_as_json("counter") {
            if last.as_ref() != Some(&v) {
                println!("← counter from server: {v}");
                last = Some(v);
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
