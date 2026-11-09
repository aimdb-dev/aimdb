//! A `SerialClient` that restarts while the board keeps running (#443).
//!
//! A UART never closes, so the server learns that the client behind it changed
//! only from a `hello`. `SerialClient` sends one on every connection: without
//! it, a restarted process numbers its subscriptions from 1 again, the old
//! session still holds those ids, and the server refuses them, so the
//! mirrored records go silent until the board is reset.

#![cfg(feature = "std")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_serial_connector::framing::{encode_frame, FrameAccumulator};
use aimdb_serial_connector::{SerialClient, SerialServer};
use aimdb_tokio_adapter::net::TokioByteStream;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temp {
    c: u64,
}

/// Where bytes the board sends go: the current host process, or nowhere
/// while none has the port open.
type HostSink = Arc<tokio::sync::Mutex<Option<WriteHalf<DuplexStream>>>>;

/// One host process: its database and the task carrying its bytes to the line.
struct Host {
    db: Arc<AimDb>,
    runner: JoinHandle<()>,
    uplink: JoinHandle<()>,
}

impl Host {
    /// Kill the process: no goodbye reaches the board, as with a closed port.
    async fn kill(self, sink: &HostSink) {
        self.runner.abort();
        self.uplink.abort();
        *sink.lock().await = None;
    }
}

/// Start a host process that mirrors the board's `temp` over the line.
async fn start_host(
    sink: &HostSink,
    line_wr: &Arc<tokio::sync::Mutex<WriteHalf<DuplexStream>>>,
) -> Host {
    let (process_end, port_end) = tokio::io::duplex(64 * 1024);
    let (mut port_rd, port_wr) = tokio::io::split(port_end);
    *sink.lock().await = Some(port_wr);
    let line_wr = line_wr.clone();
    let uplink = tokio::spawn(async move {
        let mut buf = [0u8; 256];
        while let Ok(n) = port_rd.read(&mut buf).await {
            if n == 0 || line_wr.lock().await.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    });

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(SerialClient::new(TokioByteStream(process_end)));
    builder.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("serial://temp")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Temp>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    let (db, runner) = builder.build().await.expect("build host db");
    Host {
        db: Arc::new(db),
        runner: tokio::spawn(runner.run()),
        uplink,
    }
}

/// The latest `temp` the host has mirrored, if any.
fn mirrored(db: &AimDb) -> Option<u64> {
    db.try_latest_as_json("temp")
        .and_then(|s| s.value["c"].as_u64())
}

#[tokio::test]
async fn a_restarted_client_keeps_mirroring() {
    // The line: the board holds one end for its whole life.
    let (board_end, line_end) = tokio::io::duplex(64 * 1024);

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(SerialServer::new(TokioByteStream(board_end)));
    builder.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    let (board, runner) = builder.build().await.expect("build board db");
    let board = Arc::new(board);
    tokio::spawn(runner.run());

    // The record changes all the time, so events of the old session are
    // still on the line while the next client says hello.
    let produced = Arc::new(Mutex::new(0u64));
    {
        let (board, produced) = (board.clone(), produced.clone());
        tokio::spawn(async move {
            loop {
                let c = {
                    let mut n = produced.lock().unwrap();
                    *n += 1;
                    *n
                };
                let _ = board.produce::<Temp>("temp", Temp { c });
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
    }

    // The host side of the line, which outlives every host process on it.
    let (mut line_rd, line_wr): (ReadHalf<DuplexStream>, _) = tokio::io::split(line_end);
    let line_wr = Arc::new(tokio::sync::Mutex::new(line_wr));
    let sink: HostSink = Arc::new(tokio::sync::Mutex::new(None));
    {
        let sink = sink.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = line_rd.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut sink = sink.lock().await;
                if let Some(port) = sink.as_mut() {
                    if port.write_all(&buf[..n]).await.is_err() {
                        *sink = None;
                    }
                }
            }
        });
    }

    for generation in 1..=3 {
        let host = start_host(&sink, &line_wr).await;
        let floor = *produced.lock().unwrap();
        let mut caught_up = false;
        for _ in 0..150 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if mirrored(&host.db).is_some_and(|c| c > floor) {
                caught_up = true;
                break;
            }
        }
        assert!(
            caught_up,
            "host process {generation}: the mirror never got past {floor} (latest: {:?})",
            mirrored(&host.db)
        );
        host.kill(&sink).await;
        // Let the board stream into the void for a while, as it does while
        // the host restarts.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The client sends `hello` first and subscribes without waiting for the
/// `Welcome`. When the reply does arrive, late and with id `0`, it touches no
/// subscription — not even a refusal, which is only logged.
#[tokio::test]
async fn the_hello_goes_first_and_its_reply_is_not_awaited() {
    let (board_end, process_end) = tokio::io::duplex(64 * 1024);
    let (db, host) = start_fake_host(process_end).await;
    let (mut rd, mut wr) = tokio::io::split(board_end);
    let mut acc = FrameAccumulator::new();

    let hello = next_frame(&mut rd, &mut acc).await;
    assert_eq!(
        hello["t"], "req",
        "the client opens with a hello, got {hello}"
    );
    assert_eq!(hello["method"], "hello");
    assert_eq!(hello["id"], 0);
    assert_eq!(
        hello["params"]["version"],
        aimdb_core::remote::PROTOCOL_VERSION
    );

    // No Welcome sent: the subscription follows anyway.
    let sub = next_frame(&mut rd, &mut acc).await;
    assert_eq!(sub["t"], "sub", "subscribes without a Welcome, got {sub}");
    assert_eq!(sub["topic"], "temp");
    let sub_id = sub["id"].as_u64().expect("a subscribe id");
    assert_ne!(sub_id, 0, "the subscription cannot share the hello's id");
    let sub_id = sub_id.to_string();

    let event =
        |seq: u64, c: u64| json!({"t":"event","sub":sub_id,"seq":seq,"ts":seq,"data":{"c":c}});
    send(&mut wr, event(1, 1)).await;
    assert!(mirrors(&db, 1).await, "the first event reaches the mirror");

    // The replies to the hello, late: a Welcome, then a refusal.
    send(&mut wr, json!({"t":"reply","id":0,"ok":{}})).await;
    send(
        &mut wr,
        json!({"t":"reply","id":0,"err":"version_mismatch"}),
    )
    .await;
    send(&mut wr, event(2, 2)).await;
    assert!(
        mirrors(&db, 2).await,
        "a late reply with id 0 must not end the subscription"
    );
    host.abort();
}

/// Wait up to a second for the host's mirrored `temp` to read `c`.
async fn mirrors(db: &AimDb, c: u64) -> bool {
    for _ in 0..50 {
        if mirrored(db) == Some(c) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A host process mirroring `temp` over `stream`, which a test drives by hand.
async fn start_fake_host(stream: DuplexStream) -> (Arc<AimDb>, JoinHandle<()>) {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(SerialClient::new(TokioByteStream(stream)));
    builder.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("serial://temp")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Temp>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    let (db, runner) = builder.build().await.expect("build host db");
    (Arc::new(db), tokio::spawn(runner.run()))
}

async fn send(wr: &mut WriteHalf<DuplexStream>, v: Value) {
    let mut wire = Vec::new();
    encode_frame(&serde_json::to_vec(&v).unwrap(), &mut wire);
    wr.write_all(&wire).await.unwrap();
}

async fn next_frame(rd: &mut ReadHalf<DuplexStream>, acc: &mut FrameAccumulator) -> Value {
    let mut buf = [0u8; 256];
    loop {
        if let Some(Ok(f)) = acc.next_frame() {
            return serde_json::from_slice(&f).expect("an AimX frame");
        }
        let n = tokio::time::timeout(Duration::from_secs(3), rd.read(&mut buf))
            .await
            .expect("a frame within 3s")
            .unwrap();
        assert!(n > 0, "the client closed the line");
        acc.push_bytes(&buf[..n]);
    }
}
