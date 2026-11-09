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

/// A board that loses the first `hello`, has events of an earlier session in
/// flight, and answers the second `hello`: the client waits it out and only
/// then subscribes. A board refusing the version stops the client instead.
#[tokio::test]
async fn the_hello_is_resent_and_stray_frames_are_skipped() {
    let (board_end, process_end) = tokio::io::duplex(64 * 1024);
    let host = start_fake_host(process_end).await;
    let (mut rd, mut wr) = tokio::io::split(board_end);
    let mut acc = FrameAccumulator::new();

    let first = next_frame(&mut rd, &mut acc).await;
    assert_eq!(first["method"], "hello", "the client opens with a hello");
    assert_eq!(first["id"], 0);
    // An earlier session's traffic, and no reply to the hello.
    send(&mut wr, json!({"t":"pong"})).await;
    send(&mut wr, json!({"t":"reply","id":7,"ok":null})).await;

    let started = tokio::time::Instant::now();
    let second = next_frame(&mut rd, &mut acc).await;
    assert_eq!(second["method"], "hello", "the unanswered hello is resent");
    assert!(started.elapsed() >= Duration::from_millis(500));
    send(&mut wr, json!({"t":"reply","id":0,"ok":{}})).await;

    let sub = next_frame(&mut rd, &mut acc).await;
    assert_eq!(sub["t"], "sub", "subscribes once greeted, got {sub}");
    assert_eq!(sub["topic"], "temp");
    host.abort();
}

#[tokio::test]
async fn a_refused_version_stops_the_client() {
    let (board_end, process_end) = tokio::io::duplex(64 * 1024);
    let host = start_fake_host(process_end).await;
    let (mut rd, mut wr) = tokio::io::split(board_end);
    let mut acc = FrameAccumulator::new();

    let hello = next_frame(&mut rd, &mut acc).await;
    assert_eq!(hello["method"], "hello");
    send(
        &mut wr,
        json!({"t":"reply","id":0,"err":"version_mismatch"}),
    )
    .await;

    // Neither a subscribe nor another hello follows.
    let mut buf = [0u8; 256];
    match tokio::time::timeout(Duration::from_millis(1500), rd.read(&mut buf)).await {
        Err(_) | Ok(Ok(0)) => {}
        Ok(r) => panic!(
            "the client kept talking after a refusal: {r:?} {:?}",
            &buf[..]
        ),
    }
    host.abort();
}

/// A host process mirroring `temp` over `stream`, which a test drives by hand.
async fn start_fake_host(stream: DuplexStream) -> JoinHandle<()> {
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
    let (_db, runner) = builder.build().await.expect("build host db");
    tokio::spawn(runner.run())
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
