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
//! - `set <device> [baud]` — `record.set` the writable `setting` record, then
//!   `record.get` it back (exercises the write path).
//! - `raw <device> [baud] [method] [name]` — low-level debug: send one request and
//!   print the full decoded reply (no engine), handy when `client` misbehaves.
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
        "set" => run_set_mode(device, baud).await,
        "raw" => {
            let method = args
                .get(4)
                .cloned()
                .unwrap_or_else(|| "record.list".to_string());
            let name = args
                .get(5)
                .cloned()
                .unwrap_or_else(|| "counter".to_string());
            run_raw(device, baud, method, name).await
        }
        other => {
            eprintln!(
                "usage: serial_demo <client|server|set|raw> <device> [baud] [method] [name]  (got mode '{other}')"
            );
            std::process::exit(2);
        }
    }
}

/// Low-level debug: open the port, send one AimX request for `method`, and print
/// the full reply (accumulated until the terminating `0x00`, then COBS-decoded).
/// Every step is visible — useful when the engine-based `client` just hangs.
async fn run_raw(device: String, baud: u32, method: String, name: String) {
    use aimdb_core::session::{EnvelopeCodec, Inbound};
    use aimdb_serial_connector::framing::encode_frame;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;

    println!("[raw] {method} (name={name}) over {device} @ {baud}");
    let mut port = match tokio_serial::new(&device, baud).open_native_async() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[raw] OPEN FAILED: {e}");
            std::process::exit(1);
        }
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let params = match method.as_str() {
        "record.get" => json!({ "name": name }),
        "record.set" => json!({ "name": name, "value": { "level": 42 } }),
        _ => json!({}),
    };
    let mut payload = Vec::new();
    AimxCodec
        .encode_inbound(
            Inbound::Request {
                id: 1,
                method: method.clone(),
                params: serde_json::to_vec(&params).unwrap().into(),
            },
            &mut payload,
        )
        .expect("encode request");
    let mut frame = Vec::new();
    encode_frame(&payload, &mut frame);
    println!("[raw] request: {}", String::from_utf8_lossy(&payload));
    port.write_all(&frame).await.expect("write");
    port.flush().await.expect("flush");
    println!("[raw] sent {} B; awaiting reply (5 s) …", frame.len());

    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            println!("[raw] TIMEOUT — got {} B, no terminating 0x00", acc.len());
            break;
        }
        match tokio::time::timeout(remaining, port.read(&mut buf)).await {
            Ok(Ok(0)) => {
                println!("[raw] EOF");
                break;
            }
            Ok(Ok(n)) => {
                acc.extend_from_slice(&buf[..n]);
                println!("[raw]   +{n} B (total {})", acc.len());
                if acc.contains(&0) {
                    break;
                }
            }
            Ok(Err(e)) => {
                println!("[raw] read error: {e}");
                break;
            }
            Err(_) => {
                println!("[raw] TIMEOUT");
                break;
            }
        }
    }
    if let Some(pos) = acc.iter().position(|&b| b == 0) {
        match cobs::decode_vec(&acc[..pos]) {
            Ok(json) => println!(
                "[raw] reply ({} B): {}",
                json.len(),
                String::from_utf8_lossy(&json)
            ),
            Err(_) => println!("[raw] COBS decode FAILED on {pos} B"),
        }
    }
}

/// Exercise the write path: `record.set` the `setting` record, then `record.get`
/// it back to confirm the value changed on the board.
async fn run_set_mode(device: String, baud: u32) {
    println!("[set] writing `setting` over {device} @ {baud} baud");

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

    let mut level = 0u64;
    loop {
        level += 1;
        let set_params: Payload = serde_json::to_vec(&json!({
            "name": "setting",
            "value": { "level": level }
        }))
        .unwrap()
        .into();
        match handle.call("record.set", set_params).await {
            Ok(r) => println!(
                "[set] record.set setting={{\"level\":{level}}} -> {}",
                String::from_utf8_lossy(&r)
            ),
            Err(e) => println!("[set] record.set failed: {e:?}"),
        }

        let get_params: Payload = serde_json::to_vec(&json!({ "name": "setting" }))
            .unwrap()
            .into();
        match handle.call("record.get", get_params).await {
            Ok(v) => println!(
                "[set] record.get setting    -> {}",
                String::from_utf8_lossy(&v)
            ),
            Err(e) => println!("[set] record.get failed: {e:?}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
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
