//! H-A socket-driven fan-out benchmark (design 048 WI4).
//!
//! Unlike the `aimdb-bench` encoding microbench (which isolates the codec),
//! this drives the **real** converged path end-to-end: a live WebSocket server
//! (`WebSocketConnector` + `ClientManager` + `AimxCodec` + Axum/Tokio sockets)
//! fanning one record update out to N concurrently-subscribed clients.
//!
//! The clients are **raw** `tokio-tungstenite` sockets, not aimdb client
//! connectors — 500 aimdb instances would swamp the box and put the cost on the
//! client side, whereas raw sockets keep the measured work on the server's
//! fan-out path (broadcast → per-connection encode → N socket writes).
//!
//! One measured round: set the record to a fresh value, then wait until every
//! client has observed a value ≥ that round. The elapsed time is the
//! end-to-end fan-out latency of one broadcast to N subscribers. This latency
//! **includes** the client receive/parse and the driver's poll granularity —
//! it is a system measurement, not a codec measurement.
//!
//! Run (raise the fd limit — 500 clients means ~1000 sockets in-process):
//!
//! ```bash
//! bash -c 'ulimit -n 8192; cargo bench -p aimdb-websocket-connector --bench fanout_socket'
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::WebSocketConnector;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

const SUBSCRIBER_COUNTS: [usize; 4] = [1, 10, 100, 500];
const ROUNDS: usize = 100;
const WARMUP_ROUNDS: usize = 10;
const ROUND_TIMEOUT: Duration = Duration::from_secs(15);

/// Record whose `n` field carries the round marker; `blob` sizes the payload
/// (empty for the small shape, ~1 KiB for byte-heavy — serialized as a JSON
/// integer array, the same shape the encoding microbench used).
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Rec {
    n: u64,
    blob: Vec<u8>,
}

struct Shape {
    name: &'static str,
    blob_len: usize,
}

const SHAPES: [Shape; 2] = [
    Shape {
        name: "small",
        blob_len: 0,
    },
    Shape {
        name: "byte_heavy",
        blob_len: 1024,
    },
];

fn free_addr() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn wait_for_listen(addr: SocketAddr) {
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server never bound at {addr}");
}

async fn spawn_server() -> (SocketAddr, Arc<AimDb>, JoinHandle<()>) {
    let addr = free_addr();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            WebSocketConnector::new()
                .bind(addr)
                .path("/ws")
                .with_late_join(true),
        );
    sb.configure::<Rec>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws://counter")
            .with_serializer(|_ctx, r: &Rec| Ok(serde_json::to_vec(r).unwrap()))
            .finish();
    });
    let (db, runner) = sb.build().await.expect("build server db");
    let db = Arc::new(db);
    let handle = tokio::spawn(async move {
        runner.run().await;
    });
    wait_for_listen(addr).await;
    (addr, db, handle)
}

/// Cheap scan for the record's `"n":<digits>` marker without a full JSON parse.
fn parse_n(bytes: &[u8]) -> Option<u64> {
    let needle = b"\"n\":";
    let start = bytes.windows(needle.len()).position(|w| w == needle)? + needle.len();
    let mut value: u64 = 0;
    let mut any = false;
    for &byte in &bytes[start..] {
        if byte.is_ascii_digit() {
            value = value * 10 + (byte - b'0') as u64;
            any = true;
        } else {
            break;
        }
    }
    any.then_some(value)
}

fn note_seen(seen: &AtomicU64, bytes: &[u8]) {
    if let Some(value) = parse_n(bytes) {
        seen.fetch_max(value, Ordering::Relaxed);
    }
}

/// Spawn one raw WS client: connect, subscribe, signal ready once the
/// `subscribed` ack lands, then update `seen` with every observed marker.
fn spawn_client(
    addr: SocketAddr,
    id: u64,
    seen: Arc<AtomicU64>,
    ready: mpsc::Sender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("client connect");
        ws.send(Message::Text(
            json!({"t":"sub","id":id,"topic":"counter"})
                .to_string()
                .into(),
        ))
        .await
        .expect("send subscribe");

        // Drain until the subscribe ack, then announce readiness.
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    if text.contains("\"subscribed\"") {
                        break;
                    }
                    note_seen(&seen, text.as_bytes());
                }
                Some(Ok(Message::Binary(bin))) => note_seen(&seen, &bin),
                Some(Ok(_)) => {}
                _ => return,
            }
        }
        if ready.send(()).await.is_err() {
            return;
        }

        // Event loop: record the highest marker seen.
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => note_seen(&seen, text.as_bytes()),
                Some(Ok(Message::Binary(bin))) => note_seen(&seen, &bin),
                Some(Ok(_)) => {}
                _ => return,
            }
        }
    })
}

async fn set_and_wait(db: &AimDb, seen: &[Arc<AtomicU64>], round: u64, blob: &[u8]) -> Duration {
    let start = Instant::now();
    db.set_record_from_json("counter", json!({"n": round, "blob": blob}))
        .expect("set record");
    loop {
        if seen.iter().all(|s| s.load(Ordering::Relaxed) >= round) {
            return start.elapsed();
        }
        if start.elapsed() > ROUND_TIMEOUT {
            let reached = seen
                .iter()
                .filter(|s| s.load(Ordering::Relaxed) >= round)
                .count();
            panic!(
                "round {round} timed out: {reached}/{} clients delivered",
                seen.len()
            );
        }
        tokio::task::yield_now().await;
    }
}

struct Stats {
    median_us: f64,
    p99_us: f64,
    per_delivery_ns: f64,
    throughput_msgs_per_s: f64,
}

async fn measure(blob_len: usize, subscribers: usize) -> Stats {
    let (addr, db, server) = spawn_server().await;

    let seen: Vec<Arc<AtomicU64>> = (0..subscribers)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let (ready_tx, mut ready_rx) = mpsc::channel(subscribers.max(1));
    let mut clients = Vec::with_capacity(subscribers);
    for (id, seen_slot) in seen.iter().enumerate() {
        clients.push(spawn_client(
            addr,
            id as u64,
            seen_slot.clone(),
            ready_tx.clone(),
        ));
    }
    drop(ready_tx);
    for _ in 0..subscribers {
        timeout(Duration::from_secs(30), ready_rx.recv())
            .await
            .expect("clients failed to subscribe in time")
            .expect("client dropped before ready");
    }

    let blob = vec![0u8; blob_len];
    let mut round = 0u64;
    for _ in 0..WARMUP_ROUNDS {
        round += 1;
        set_and_wait(&db, &seen, round, &blob).await;
    }

    let mut latencies = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        round += 1;
        latencies.push(set_and_wait(&db, &seen, round, &blob).await);
    }

    for client in clients {
        client.abort();
    }
    server.abort();

    latencies.sort_unstable();
    let median = latencies[latencies.len() / 2];
    let p99 = latencies[(latencies.len() * 99 / 100).min(latencies.len() - 1)];
    let total: Duration = latencies.iter().sum();
    let deliveries = (latencies.len() * subscribers) as f64;
    Stats {
        median_us: median.as_secs_f64() * 1e6,
        p99_us: p99.as_secs_f64() * 1e6,
        per_delivery_ns: median.as_secs_f64() * 1e9 / subscribers as f64,
        throughput_msgs_per_s: deliveries / total.as_secs_f64(),
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("=== H-A socket-driven fan-out ({ROUNDS} rounds/config) ===");
    println!(
        "{:<12} {:>5} {:>14} {:>14} {:>16} {:>16}",
        "shape", "subs", "median (µs)", "p99 (µs)", "per-delivery ns", "msgs/s"
    );
    for shape in SHAPES {
        for subscribers in SUBSCRIBER_COUNTS {
            let stats = measure(shape.blob_len, subscribers).await;
            println!(
                "{:<12} {:>5} {:>14.1} {:>14.1} {:>16.0} {:>16.0}",
                shape.name,
                subscribers,
                stats.median_us,
                stats.p99_us,
                stats.per_delivery_ns,
                stats.throughput_msgs_per_s,
            );
        }
    }
}
