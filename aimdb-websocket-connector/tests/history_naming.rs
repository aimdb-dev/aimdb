//! A record's history is stored and authorized under its record key (#426).
//!
//! Live reads are granted on record keys. `record.query` filters rows by their
//! `topic`, which is the name the rows were stored under. `.persist()` stores
//! rows under the record key, so both reads are checked against the same name.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_persistence::{
    AimDbBuilderPersistExt, BoxFuture, PersistenceBackend, PersistenceError, QueryParams,
    RecordRegistrarPersistExt, StoredValue,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, Permissions, WebSocketConnector,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

// ── In-memory persistence backend ────────────────────────────────────

/// Keeps every stored row; `query` filters by MQTT pattern and time bounds.
#[derive(Default)]
struct MemBackend {
    rows: Mutex<Vec<StoredValue>>,
}

impl PersistenceBackend for MemBackend {
    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        self.rows.lock().unwrap().push(StoredValue {
            record_name: record_name.into(),
            value: value.clone(),
            stored_at: timestamp,
        });
        Box::pin(async { Ok(()) })
    }

    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        let rows: Vec<StoredValue> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| aimdb_core::topic_matches(record_pattern, &r.record_name))
            .filter(|r| params.start_time.is_none_or(|s| r.stored_at >= s))
            .filter(|r| params.end_time.is_none_or(|e| r.stored_at <= e))
            .cloned()
            .collect();
        Box::pin(async move { Ok(rows) })
    }

    fn cleanup(&self, _older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Ok(0) })
    }
}

// ── Auth: `?grant=<pattern>` becomes the client's single read pattern ─

struct GrantFromQuery;
impl AuthHandler for GrantFromQuery {
    fn authenticate<'a>(
        &'a self,
        request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        let grant = request.query_params.get("grant").cloned();
        Box::pin(async move {
            Ok(match grant {
                Some(g) => Permissions {
                    read_patterns: vec![g],
                    write_patterns: Vec::new(),
                },
                None => Permissions::allow_all(),
            })
        })
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temp {
    celsius: f64,
}

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

/// A database with a WebSocket server and the real `with_persistence` query
/// handler over `backend`.
async fn spawn(
    backend: Arc<MemBackend>,
    configure: impl FnOnce(&mut AimDbBuilder),
) -> (SocketAddr, AimDb) {
    let addr = free_addr();
    let ws = WebSocketConnector::new().with_auth(GrantFromQuery);
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"))
        .with_persistence(
            backend as Arc<dyn PersistenceBackend>,
            Duration::from_secs(3600),
        );
    configure(&mut sb);
    let (db, runner) = sb.build().await.expect("build db");
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;
    (addr, db)
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: SocketAddr, grant: &str) -> WsClient {
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    tokio_tungstenite::connect_async(format!("{url}&grant={grant}"))
        .await
        .expect("connect")
        .0
}

/// One `req` → the `ok` payload of its `reply`.
async fn ws_call(c: &mut WsClient, method: &str, params: Value) -> Value {
    let frame = json!({"t": "req", "id": 1, "method": method, "params": params});
    c.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    loop {
        let msg = timeout(Duration::from_secs(3), c.next())
            .await
            .expect("recv timed out");
        let v: Value = match msg {
            Some(Ok(Message::Text(t))) => serde_json::from_str(&t).unwrap(),
            Some(Ok(Message::Binary(b))) => serde_json::from_slice(&b).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        };
        if v["t"] != "reply" {
            continue;
        }
        return v
            .get("ok")
            .cloned()
            .unwrap_or_else(|| panic!("{method} failed: {v}"));
    }
}

// ── Test ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn history_is_stored_and_authorized_under_record_key() {
    let backend = Arc::new(MemBackend::default());
    let (addr, db) = spawn(backend.clone(), |sb| {
        sb.configure::<Temp>("temp.vienna", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .with_remote_access()
                .persist();
        });
    })
    .await;

    db.set_record_from_json("temp.vienna", json!({"celsius": 21.5}))
        .expect("write");
    for _ in 0..200 {
        if !backend.rows.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        backend.rows.lock().unwrap()[0].record_name,
        "temp.vienna",
        "rows are stored under the record key"
    );

    // Granted on the record key: the history is readable.
    let mut by_key = ws_connect(addr, "temp.*").await;
    let history = ws_call(&mut by_key, "record.query", json!({"name": "#"})).await;
    let records = history["records"].as_array().expect("records array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["topic"], "temp.vienna");

    // Granted on something else: no rows.
    let mut other = ws_connect(addr, "other.*").await;
    let history = ws_call(&mut other, "record.query", json!({"name": "#"})).await;
    assert_eq!(history, json!({"records": [], "total": 0}));
}
