//! `get_record` and `drain_record` share one serial session (#450).
//!
//! A UART never closes, so a board learns of a new client only from a `hello`,
//! and a `hello` resets its session, drain cursors included. If `get_record`
//! opened a second connection, it would either fail (the tty is already open)
//! or say `hello` again and lose the cursor `drain_record` reads.
//!
//! The board is a `SerialServer` behind a pty; `aimdb-mcp` opens the other
//! side as `serial://…`. A relay between the pty and the board counts the
//! `hello`s the board receives.

#![cfg(all(feature = "transport-serial", target_os = "linux"))]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_mcp::tools;
use aimdb_serial_connector::framing::FrameAccumulator;
use aimdb_serial_connector::SerialServer;
use aimdb_tokio_adapter::net::TokioByteStream;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reading {
    n: u64,
}

/// A pty pair: the master (the board's wire) and the slave's path (the
/// host's tty). The slave fd is held open so the master never sees a
/// hang-up between host opens.
struct Pty {
    master: File,
    path: String,
    _slave: OwnedFd,
}

fn openpty() -> Pty {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: `openpty` writes two fresh fds; null name/termios/winsize are
    // allowed.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both fds were just returned by `openpty` and are owned here.
    let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let path = std::fs::read_link(format!("/proc/self/fd/{}", {
        use std::os::fd::AsRawFd;
        slave.as_raw_fd()
    }))
    .unwrap()
    .to_string_lossy()
    .into_owned();
    Pty {
        master,
        path,
        _slave: slave,
    }
}

/// Relay bytes between the pty master and the board's end of a duplex,
/// counting the `hello` requests on the way to the board. Blocking pty I/O
/// runs on plain threads, so nothing left blocked can stall the runtime.
fn relay(master: File, board_line: tokio::io::DuplexStream, hellos: Arc<AtomicUsize>) {
    let (mut line_rd, mut line_wr) = tokio::io::split(board_line);
    let mut master_wr = master.try_clone().unwrap();
    let mut master_rd = master;

    // host → board
    let (to_board, mut to_board_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut frames = FrameAccumulator::new();
        let mut buf = [0u8; 512];
        while let Ok(n) = master_rd.read(&mut buf) {
            if n == 0 {
                break;
            }
            frames.push_bytes(&buf[..n]);
            while let Some(frame) = frames.next_frame() {
                let Ok(frame) = frame else { continue };
                let msg: Value = serde_json::from_slice(&frame).unwrap_or(Value::Null);
                if msg["method"] == "hello" {
                    hellos.fetch_add(1, SeqCst);
                }
            }
            if to_board.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(bytes) = to_board_rx.recv().await {
            if line_wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    // board → host
    let (to_host, to_host_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        while let Ok(n) = line_rd.read(&mut buf).await {
            if n == 0 || to_host.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        while let Ok(bytes) = to_host_rx.recv() {
            if master_wr.write_all(&bytes).is_err() {
                break;
            }
        }
    });
}

#[tokio::test]
async fn get_record_shares_the_drain_session_over_serial() {
    // The board: a ring of events and a state record, served over the line.
    let (board_end, line_end) = tokio::io::duplex(64 * 1024);
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(SerialServer::new(TokioByteStream(board_end)));
    builder.configure::<Reading>("events", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    builder.configure::<Reading>("state", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (board, runner) = builder.build().await.expect("build board");
    tokio::spawn(runner.run());
    board
        .producer::<Reading>("state")
        .unwrap()
        .produce(Reading { n: 42 });

    let pty = openpty();
    let hellos = Arc::new(AtomicUsize::new(0));
    relay(pty.master, line_end, hellos.clone());

    tools::init_connection_pool(aimdb_mcp::connection::ConnectionPool::new());
    let endpoint = format!("serial://{}?baud=115200", pty.path);
    let drain = || {
        tools::drain_record(Some(
            json!({ "endpoint": endpoint, "record_name": "events" }),
        ))
    };
    let get =
        |name: &str| tools::get_record(Some(json!({ "endpoint": endpoint, "record_name": name })));

    // 1. Open the ring's drain cursor.
    let first = drain().await.expect("first drain");
    assert_eq!(first["count"], 0);

    // 2. The board publishes events.
    let events = board.producer::<Reading>("events").unwrap();
    for n in 1..=3 {
        events.produce(Reading { n });
    }

    // 3. Read other records on the same session, misses included.
    let got = get("state").await.expect("get_record over serial");
    assert_eq!(got["value"], json!({ "n": 42 }));
    let err = get("missing")
        .await
        .expect_err("unknown record")
        .to_string();
    assert!(err.contains("not_found"), "{err}");
    let err = get("events").await.expect_err("a ring").to_string();
    assert!(err.contains("unsupported"), "{err}");
    // An error reply from drain keeps the session too.
    let err = tools::drain_record(Some(
        json!({ "endpoint": endpoint, "record_name": "missing" }),
    ))
    .await
    .expect_err("unknown record")
    .to_string();
    assert!(err.contains("not_found"), "{err}");
    // Let any deferred cleanup run before the session is used again.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 4. The cursor survived: the drain returns the queued events.
    let mut ns = Vec::new();
    for _ in 0..100 {
        let page = drain().await.expect("drain");
        ns.extend(
            page["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["value"]["n"].clone()),
        );
        if ns.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(ns, [1, 2, 3]);

    // 5. One session the whole time, still usable.
    let got = get("state").await.expect("get_record after the drains");
    assert_eq!(got["value"], json!({ "n": 42 }));
    assert_eq!(
        hellos.load(SeqCst),
        1,
        "the board received exactly one hello"
    );
}
