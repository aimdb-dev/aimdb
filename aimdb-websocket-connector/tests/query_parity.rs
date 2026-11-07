//! `record.query` gives the same reply over AimX and WebSocket (#422, #429).
//!
//! One database, served by the real `with_persistence` handler over an
//! in-memory backend. AimX is driven through `AimxDispatch` (what UDS / TCP /
//! serial run), WebSocket through a real client socket.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::AimxDispatch;
use aimdb_core::session::{Dispatch, SessionCtx};
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_persistence::{
    AimDbBuilderPersistExt, BoxFuture, PersistenceBackend, PersistenceError, QueryParams,
    StoredValue,
};
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, Permissions, WebSocketConnector,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// Pattern match, inclusive time bounds and the newest `limit` rows per
/// record, as `SqliteBackend` does.
#[derive(Default)]
struct MemBackend(Mutex<Vec<StoredValue>>);

impl MemBackend {
    fn seed(&self, name: &str, value: Value, ts: u64) {
        self.0.lock().unwrap().push(StoredValue {
            record_name: name.into(),
            value,
            stored_at: ts,
        });
    }
}

impl PersistenceBackend for MemBackend {
    fn store<'a>(
        &'a self,
        _: &'a str,
        _: &'a Value,
        _: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    fn query<'a>(
        &'a self,
        pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        let mut rows: Vec<StoredValue> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|r| aimdb_core::topic_matches(pattern, &r.record_name))
            .filter(|r| params.start_time.is_none_or(|s| r.stored_at >= s))
            .filter(|r| params.end_time.is_none_or(|e| r.stored_at <= e))
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.stored_at));
        if let Some(limit) = params.limit_per_record {
            let mut kept: Vec<StoredValue> = Vec::new();
            for row in rows {
                if kept
                    .iter()
                    .filter(|k| k.record_name == row.record_name)
                    .count()
                    < limit
                {
                    kept.push(row);
                }
            }
            rows = kept;
        }
        Box::pin(async move { Ok(rows) })
    }

    fn cleanup(&self, _: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Ok(0) })
    }
}

/// `?grant=<pattern>` becomes the client's only read pattern.
struct GrantFromQuery;
impl AuthHandler for GrantFromQuery {
    fn authenticate<'a>(
        &'a self,
        request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        let grant = request.query_params["grant"].clone();
        Box::pin(async move {
            Ok(Permissions {
                read_patterns: vec![grant],
                write_patterns: Vec::new(),
            })
        })
    }
}

/// A database with seeded history, serving WebSocket on a free port.
async fn spawn() -> (SocketAddr, Arc<AimDb>) {
    let backend = Arc::new(MemBackend::default());
    for (name, value, ts) in [
        ("sensors.temp", json!(20.0), 100),
        ("sensors.temp", json!(21.0), 300),
        ("sensors.temp", json!(22.0), 500),
        ("sensors.hum", json!(40), 200),
        ("sensors.hum", json!(41), 400),
        ("sensors.deep.x", json!("d"), 250),
        ("ledger", json!(1), 150),
        ("ledger", json!(2), 350),
    ] {
        backend.seed(name, value, ts);
    }

    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let ws = WebSocketConnector::new().with_auth(GrantFromQuery);
    let (db, runner) = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"))
        .with_persistence(backend, Duration::from_secs(3600))
        .build()
        .await
        .expect("build db");
    tokio::spawn(runner.run());
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_ok() {
            return (addr, Arc::new(db));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server never bound at {addr}");
}

async fn aimx_query(db: &Arc<AimDb>, params: &Value) -> Value {
    let dispatch = AimxDispatch::new(db.clone(), AimxConfig::uds_default());
    let mut session = dispatch.open(&SessionCtx::default());
    let payload = serde_json::to_vec(params).unwrap();
    let reply = session
        .call("record.query", payload.as_slice().into())
        .await
        .expect("aimx query");
    serde_json::from_slice(&reply).unwrap()
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: SocketAddr, grant: &str) -> WsClient {
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    tokio_tungstenite::connect_async(format!("{url}&grant={}", grant.replace('#', "%23")))
        .await
        .expect("connect")
        .0
}

async fn ws_query(ws: &mut WsClient, params: &Value) -> Value {
    let req = json!({"t": "req", "id": 1, "method": "record.query", "params": params});
    ws.send(Message::Text(req.to_string().into()))
        .await
        .unwrap();
    loop {
        let msg = timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("reply timed out");
        let Some(Ok(Message::Text(t))) = msg else {
            panic!("unexpected frame: {msg:?}");
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["t"] == "reply" {
            return v.get("ok").cloned().expect("ws query");
        }
    }
}

/// `(topic, ts)` of every row, in reply order.
fn rows(reply: &Value) -> Vec<(String, u64)> {
    reply["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|r| {
            (
                r["topic"].as_str().unwrap().into(),
                r["ts"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn r(topic: &str, ts: u64) -> (String, u64) {
    (topic.into(), ts)
}

#[tokio::test]
async fn both_transports_give_the_same_reply() {
    let (addr, db) = spawn().await;
    let mut ws = ws_connect(addr, "#").await;

    let cases = [
        // Omitted name → `#`; omitted limit → newest row per record.
        (
            json!({}),
            vec![
                r("sensors.deep.x", 250),
                r("ledger", 350),
                r("sensors.hum", 400),
                r("sensors.temp", 500),
            ],
        ),
        // `*` is one segment.
        (json!({"name": "*"}), vec![r("ledger", 350)]),
        (
            json!({"name": "sensors.#"}),
            vec![
                r("sensors.deep.x", 250),
                r("sensors.hum", 400),
                r("sensors.temp", 500),
            ],
        ),
        // Per-record limit keeps the newest rows.
        (
            json!({"name": "sensors.temp", "limit": 2}),
            vec![r("sensors.temp", 300), r("sensors.temp", 500)],
        ),
        // Inclusive bounds, ordered by `ts` across records.
        (
            json!({"limit": 10, "start": 200, "end": 400}),
            vec![
                r("sensors.hum", 200),
                r("sensors.deep.x", 250),
                r("sensors.temp", 300),
                r("ledger", 350),
                r("sensors.hum", 400),
            ],
        ),
        (json!({"name": "nothing.here"}), vec![]),
    ];

    for (params, expected) in cases {
        let via_aimx = aimx_query(&db, &params).await;
        let via_ws = ws_query(&mut ws, &params).await;
        assert_eq!(via_aimx, via_ws, "transports disagree on {params}");
        assert_eq!(rows(&via_aimx), expected, "rows for {params}");
        assert_eq!(
            via_aimx.as_object().unwrap().len(),
            1,
            "only `records` in the reply"
        );
    }
}

/// A client granted `sensors.*` gets only those rows, and each granted record
/// keeps its full per-record quota.
#[tokio::test]
async fn restricted_client_sees_only_granted_rows() {
    let (addr, _db) = spawn().await;
    let mut ws = ws_connect(addr, "sensors.*").await;

    let reply = ws_query(&mut ws, &json!({"name": "#", "limit": 10})).await;
    assert_eq!(
        rows(&reply),
        [
            r("sensors.temp", 100),
            r("sensors.hum", 200),
            r("sensors.temp", 300),
            r("sensors.hum", 400),
            r("sensors.temp", 500),
        ]
    );
}
