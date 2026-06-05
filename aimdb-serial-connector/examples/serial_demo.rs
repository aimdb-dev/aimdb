//! Real-serial demo for `aimdb-serial-connector` — talk AimX over an actual
//! serial device (a board's ST-LINK Virtual COM Port, or a `socat` PTY pair for a
//! no-hardware smoke).
//!
//! Two modes, so either end can be the host:
//!
//! ```text
//! # board (Embassy SerialServer) ⇄ host:
//! cargo run --example serial_demo --features _test-tokio -- client /dev/ttyACM0
//!
//! # host SerialServer ⇄ host client over a PTY (no hardware):
//! socat -d -d pty,raw,echo=0 pty,raw,echo=0          # prints two /dev/pts/N
//! cargo run ... -- server /dev/pts/3
//! cargo run ... -- client /dev/pts/4
//! ```
//!
//! - `server <device> [baud]` — serve a live `counter` record over the line.
//! - `client <device> [baud]` — `record.list` once, then `record.get counter`
//!   every second, printing what comes back across the wire.
//!
//! Built only under the internal `_test-tokio` feature (it needs a concrete
//! adapter); see the crate's `Cargo.toml`.
//!
//! On macOS the board's VCP is `/dev/cu.usbmodem…` (use the `cu.*`, not `tty.*`,
//! node). Run from the workspace root, and make sure nothing else holds the port
//! (no stray `screen`/client) — only one program may open it at a time.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{run_client, ClientConfig, Payload};
use aimdb_core::AimDbBuilder;
use aimdb_serial_connector::tokio_transport::{SerialDialer, SerialServer};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Counter {
    value: u64,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("client");
    let device = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "/dev/ttyACM0".to_string());
    let baud: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(115_200);

    match mode {
        "server" => run_server(device, baud).await,
        "client" => run_client_mode(device, baud).await,
        other => {
            eprintln!("usage: serial_demo <client|server> <device> [baud]  (got mode '{other}')");
            std::process::exit(2);
        }
    }
}

/// Serve a `counter` record (incrementing once per second) over the serial line.
async fn run_server(device: String, baud: u32) {
    println!("[server] serving AimX over {device} @ {baud} baud (Ctrl-C to stop)");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(SerialServer::new(device, baud).with_config(AimxConfig::uds_default()));
    builder.configure::<Counter>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);

    // Drive a value into the record once per second.
    let producer = db.producer::<Counter>("counter").expect("producer");
    tokio::spawn(async move {
        let mut value = 0u64;
        loop {
            value += 1;
            producer.produce(Counter { value });
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    // The runner drives the serial `serve` loop; never returns.
    runner.run().await;
}

/// Connect over the serial line and poll the `counter` record.
async fn run_client_mode(device: String, baud: u32) {
    println!("[client] querying AimX over {device} @ {baud} baud");

    let (handle, engine) = run_client(
        SerialDialer::new(device, baud),
        AimxCodec,
        ClientConfig {
            sends_hello: false,
            ..ClientConfig::default()
        },
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);

    match handle.call("record.list", Payload::from(&b"{}"[..])).await {
        Ok(list) => println!("[client] record.list = {}", String::from_utf8_lossy(&list)),
        Err(e) => println!("[client] record.list failed: {e:?}"),
    }

    loop {
        let params: Payload = serde_json::to_vec(&json!({ "name": "counter" }))
            .unwrap()
            .into();
        match handle.call("record.get", params).await {
            Ok(v) => println!("[client] counter = {}", String::from_utf8_lossy(&v)),
            Err(e) => println!("[client] record.get failed: {e:?}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
