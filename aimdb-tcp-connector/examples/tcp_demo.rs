//! Small Tokio demo for `aimdb-tcp-connector`.
//!
//! Run from the workspace root:
//!
//! ```text
//! # terminal 1
//! cargo run -p aimdb-tcp-connector --example tcp_demo --features _test-tokio -- server 127.0.0.1:7001
//!
//! # terminal 2
//! cargo run -p aimdb-tcp-connector --example tcp_demo --features _test-tokio -- client 127.0.0.1:7001
//!
//! # write a remote setting, then read it back
//! cargo run -p aimdb-tcp-connector --example tcp_demo --features _test-tokio -- set 127.0.0.1:7001 42
//! ```
//!
//! This is intentionally host-only. The first TCP PR includes Embassy client
//! support; Embassy server/examples are follow-up work because they need an
//! explicit socket/buffer-pool design.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{run_client, ClientConfig, Payload};
use aimdb_core::AimDbBuilder;
use aimdb_tcp_connector::tokio_transport::{TcpDialer, TcpServer};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Counter {
    value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Setting {
    level: u64,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("client");
    let endpoint = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:7001".to_string());

    match mode {
        "server" => run_server(endpoint).await,
        "client" => run_client_mode(endpoint).await,
        "set" => {
            let level = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(42);
            run_set_mode(endpoint, level).await;
        }
        other => {
            eprintln!("usage: tcp_demo <server|client|set> <host:port> [level]  (got '{other}')");
            std::process::exit(2);
        }
    }
}

async fn run_server(bind_addr: String) {
    println!("[server] serving AimX over tcp://{bind_addr} (Ctrl-C to stop)");

    let mut policy = SecurityPolicy::read_write();
    policy.allow_write_key("setting");
    let config = AimxConfig::uds_default()
        .security_policy(policy)
        .max_connections(8)
        .max_subs_per_connection(32);

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(TcpServer::new(bind_addr).with_config(config));
    builder.configure::<Counter>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    builder.configure::<Setting>("setting", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    let (db, runner) = builder.build().await.expect("build db");
    let db = Arc::new(db);
    db.set_record_from_json("setting", json!({ "level": 0 }))
        .expect("seed setting");

    let producer = db.producer::<Counter>("counter").expect("producer");
    tokio::spawn(async move {
        let mut value = 0u64;
        loop {
            value += 1;
            producer.produce(Counter { value });
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    runner.run().await;
}

async fn run_client_mode(endpoint: String) {
    println!("[client] querying AimX over tcp://{endpoint}");
    let handle = connect(endpoint);

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

async fn run_set_mode(endpoint: String, level: u64) {
    println!("[set] writing setting.level={level} over tcp://{endpoint}");
    let handle = connect(endpoint);

    let set_params: Payload = serde_json::to_vec(&json!({
        "name": "setting",
        "value": { "level": level }
    }))
    .unwrap()
    .into();
    match handle.call("record.set", set_params).await {
        Ok(v) => println!("[set] record.set = {}", String::from_utf8_lossy(&v)),
        Err(e) => println!("[set] record.set failed: {e:?}"),
    }

    let get_params: Payload = serde_json::to_vec(&json!({ "name": "setting" }))
        .unwrap()
        .into();
    match handle.call("record.get", get_params).await {
        Ok(v) => println!("[set] record.get = {}", String::from_utf8_lossy(&v)),
        Err(e) => println!("[set] record.get failed: {e:?}"),
    }
}

fn connect(endpoint: String) -> aimdb_core::session::ClientHandle {
    let (handle, engine) = run_client(
        TcpDialer::new(endpoint),
        AimxCodec,
        ClientConfig {
            sends_hello: false,
            ..ClientConfig::default()
        },
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);
    handle
}
