//! The MCP keeps one AimX client per endpoint and replaces it only when its
//! engine has stopped, never because a call returned an error.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::AimDbBuilder;
use aimdb_mcp::connection::ConnectionPool;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct State {
    n: u64,
}

/// An AimX server in its own runtime on its own thread. Dropping it shuts that
/// runtime down, which drops every session, as a crashed or rebooted instance
/// would.
struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(sock: &Path) -> Self {
        let sock: PathBuf = sock.to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let _ = std::fs::remove_file(&sock);
                let config = AimxConfig::uds_default().socket_path(sock.to_str().unwrap());
                let mut builder = AimDbBuilder::new()
                    .runtime(Arc::new(TokioAdapter))
                    .with_connector(UdsServer::from_config(config));
                builder.configure::<State>("state", |reg| {
                    reg.buffer(BufferCfg::SingleLatest).with_remote_access();
                });
                let (db, runner) = builder.build().await.expect("build");
                db.producer::<State>("state")
                    .unwrap()
                    .produce(State { n: 1 });
                tokio::spawn(runner.run());
                while !sock.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                ready_tx.send(()).unwrap();
                let _ = stop_rx.await;
                drop(db);
            });
            rt.shutdown_timeout(Duration::from_secs(1));
        });
        ready_rx.recv().unwrap();
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.thread.take().unwrap().join().unwrap();
    }
}

#[tokio::test]
async fn the_client_is_kept_across_error_replies_and_replaced_once_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");
    let endpoint = sock.to_str().unwrap().to_string();
    let pool = ConnectionPool::new();

    let server = Server::start(&sock);
    let first = pool.get_client(&endpoint).await.expect("connect");

    // Error replies are answers on a healthy session: the client stays.
    for missing in ["missing", "also-missing"] {
        assert!(first.lock().await.get_record(missing).await.is_err());
        let again = pool.get_client(&endpoint).await.unwrap();
        assert!(Arc::ptr_eq(&first, &again), "kept after an error reply");
    }
    assert!(!first.lock().await.is_closed());

    // The instance goes away: the engine stops.
    drop(server);
    let mut stopped = false;
    for _ in 0..200 {
        if first.lock().await.is_closed() {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stopped, "the engine stops when the transport closes");

    // The instance is back: the next call replaces the stopped client.
    let _server = Server::start(&sock);
    let next = pool.get_client(&endpoint).await.expect("reconnect");
    assert!(!Arc::ptr_eq(&first, &next), "a stopped client is replaced");
    let got = next.lock().await.get_record("state").await.expect("get");
    assert_eq!(got.value["n"], 1);
}

#[tokio::test]
async fn the_records_resource_uses_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");
    let endpoint = sock.to_str().unwrap().to_string();
    let pool = ConnectionPool::new();
    aimdb_mcp::tools::init_connection_pool(pool.clone());

    let _server = Server::start(&sock);
    aimdb_mcp::resources::records::read_records_resource(&endpoint)
        .await
        .expect("read the records resource");
    assert_eq!(pool.connection_count().await, 1, "the pool made the client");
}
