//! Real-socket integration tests for the WebSocket connector — black-box.
//!
//! These drive the connector through its **public API only**: a server `AimDb`
//! stood up with [`WebSocketConnector`] over a real TCP socket, talked to by a
//! raw `tokio-tungstenite` client speaking AimX frames (or the public
//! `run_client` + [`WsDialer`] engine). Server→client data is pushed by
//! *producing a record* — an "injector" record whose dynamic topic + raw
//! serializer let a test broadcast an arbitrary `(topic, payload)` through the
//! real `pump_sink` → bus → session path.
//!
//! The parity block at the bottom locks the AimX WS wire to the semantics the
//! retired ws-protocol offered (subscribe ack, wildcard fan-out, late-join
//! snapshot, query, list).
//!
//! Needs both halves (`server` + `client`); compiles away otherwise.

#![cfg(all(feature = "server", feature = "client"))]

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::TopicProvider;
use aimdb_core::remote::QueryHandlerFn;
use aimdb_core::session::{aimx::AimxCodec, run_client, ClientConfig};
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_data_contracts::{SchemaType, Streamable};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::transport::WsDialer;
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, ClientInfo, Permissions, QueryFuture, QueryHandler,
    QueryRecord, WebSocketConnector,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{http::StatusCode, Error, Message};

// ── Injector record ──────────────────────────────────────────────────
// Producing one pushes `payload` out on `topic` via the real outbound path.

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Inject {
    topic: String,
    payload: Value,
}

struct InjectTopic;
impl TopicProvider<Inject> for InjectTopic {
    fn topic(&self, v: &Inject) -> Option<String> {
        Some(v.topic.clone())
    }
}

// ── A registered Streamable type (for the `record.list` schema name) ──

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temp {
    c: f32,
}
impl SchemaType for Temp {
    const NAME: &'static str = "temperature";
}
impl Streamable for Temp {}

// ── A record type the connector is never told about ──────────────────
// No `register::<Ledger>()`, so it carries no `schema_type` — but a granted
// client still enumerates it: registration resolves names, it is not an ACL.

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Ledger {
    balance: i64,
}

// ── Auth + query fixtures ────────────────────────────────────────────

struct DenyAuth;
impl AuthHandler for DenyAuth {
    fn authenticate<'a>(
        &'a self,
        _request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        Box::pin(async { Err(AuthError::new("denied")) })
    }
}

/// Allows the connection (allow-all permissions) but asynchronously *denies*
/// `secret/*` via `authorize_subscribe`. If the engine only consulted the static
/// permission set, `secret` would be allowed — so this proves the async hook gates.
struct AsyncTopicAuth;
impl AuthHandler for AsyncTopicAuth {
    fn authenticate<'a>(
        &'a self,
        _request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        Box::pin(async { Ok(Permissions::allow_all()) })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client: &'a ClientInfo,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let denied = topic.starts_with("secret");
        Box::pin(async move {
            tokio::task::yield_now().await; // simulate an async ACL lookup
            !denied
        })
    }
}

/// Grants everything to a client that asks via `?grant=all`, nothing to anyone
/// else — one server, two very differently privileged clients. Only
/// `authenticate` is overridden, so the read paths ride the trait defaults.
struct GrantByQuery;
impl AuthHandler for GrantByQuery {
    fn authenticate<'a>(
        &'a self,
        request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        let all = request
            .query_params
            .get("grant")
            .is_some_and(|g| g == "all");
        Box::pin(async move {
            Ok(if all {
                Permissions::allow_all()
            } else {
                Permissions::default()
            })
        })
    }
}

struct OneRecordQuery;
impl QueryHandler for OneRecordQuery {
    fn handle_query<'a>(
        &'a self,
        _pattern: &'a str,
        _from: Option<u64>,
        _to: Option<u64>,
        _limit: Option<usize>,
    ) -> QueryFuture<'a> {
        Box::pin(async {
            Ok((
                vec![QueryRecord {
                    topic: "temp".into(),
                    payload: json!(21.0),
                    ts: 7,
                }],
                1,
            ))
        })
    }
}

// ── Harness ──────────────────────────────────────────────────────────

/// Reserve an ephemeral port, then free it so the server can bind it.
fn free_addr() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

/// Wait until the server is accepting connections at `addr`.
async fn wait_for_listen(addr: SocketAddr) {
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server never bound at {addr}");
}

/// Stand up a WS server (with the injector record) on an ephemeral port. The
/// caller pre-configures `ws` (auth / late-join / …); we add `bind`/`path`.
async fn spawn(ws: WebSocketConnector) -> (SocketAddr, Arc<AimDb>) {
    let addr = free_addr();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"));
    sb.configure::<Inject>("inject", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 1024 })
            .with_remote_access()
            .link_to("ws://_") // overridden per-value by the topic provider
            .with_topic_provider(InjectTopic)
            .with_serializer(|_ctx, m: &Inject| {
                Ok(serde_json::to_vec(&m.payload).expect("serialize payload"))
            })
            .finish();
    });
    let (db, runner) = sb.build().await.expect("build server db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;
    (addr, db)
}

/// Default allow-all, late-join-on server.
async fn spawn_default() -> (SocketAddr, Arc<AimDb>) {
    spawn(WebSocketConnector::new().with_late_join(true)).await
}

/// Push `payload` to subscribers of `topic` (one outbound record update).
fn inject(db: &AimDb, topic: &str, payload: Value) {
    db.set_record_from_json("inject", json!({ "topic": topic, "payload": payload }))
        .expect("inject");
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: SocketAddr) -> WsClient {
    ws_connect_with(addr, "").await
}

/// [`ws_connect`] with `extra` query params appended after the version
/// (e.g. `"&grant=all"`), so one server can hand clients different grants.
async fn ws_connect_with(addr: SocketAddr, extra: &str) -> WsClient {
    // Every real client declares its AimX version at the upgrade; go through the
    // shared helper so the tests exercise the exact URL the dialers produce.
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    tokio_tungstenite::connect_async(format!("{url}{extra}"))
        .await
        .expect("connect")
        .0
}

/// One-shot HTTP/1.1 `GET path` on the connector's port, returning
/// `(status_line, body)`. The plain-HTTP routes (`/health`, `/version`) have no
/// WebSocket to speak through, and the crate carries no HTTP client — a raw
/// socket keeps the test black-box without a dev-dependency.
async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut sock = TcpStream::connect(addr).await.expect("connect");
    sock.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .expect("write request");
    let mut raw = String::new();
    timeout(Duration::from_secs(3), sock.read_to_string(&mut raw))
        .await
        .expect("response timed out")
        .expect("read response");
    let (head, body) = raw.split_once("\r\n\r\n").expect("no header/body split");
    let status = head.lines().next().unwrap_or_default().to_string();
    (status, body.to_string())
}

/// The live client count `/health` reports.
async fn health_clients(addr: SocketAddr) -> usize {
    let (status, body) = http_get(addr, "/health").await;
    assert!(
        status.contains("200"),
        "unexpected /health status: {status}"
    );
    let parsed: Value = serde_json::from_str(&body).expect("health body is JSON");
    parsed["clients"].as_u64().expect("clients field") as usize
}

/// [`health_clients`] polled until it reports `want`, returning the last value
/// seen. A slot is *claimed* synchronously at the upgrade, so the admit side
/// needs no wait — but it is *released* when the session future unwinds, one
/// scheduler hop after the peer's close, so the disconnect side does.
async fn health_clients_reaches(addr: SocketAddr, want: usize) -> usize {
    let mut seen = health_clients(addr).await;
    for _ in 0..200 {
        if seen == want {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        seen = health_clients(addr).await;
    }
    seen
}

/// Send one raw AimX frame (a JSON value) as a WS text message.
async fn ws_send(c: &mut WsClient, frame: Value) {
    c.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

/// Read the next AimX frame as JSON, with a timeout so a hang fails loudly.
async fn ws_recv(c: &mut WsClient) -> Value {
    loop {
        match timeout(Duration::from_secs(3), c.next())
            .await
            .expect("recv timed out")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(Message::Binary(b))) => return serde_json::from_slice(&b).unwrap(),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

/// Read frames until one has `"t" == tag`; panics on timeout.
async fn ws_recv_tag(c: &mut WsClient, tag: &str) -> Value {
    for _ in 0..50 {
        let v = ws_recv(c).await;
        if v["t"] == tag {
            return v;
        }
    }
    panic!("no '{tag}' frame arrived");
}

// ── Server e2e ───────────────────────────────────────────────────────

#[tokio::test]
async fn server_subscribe_ack_and_wildcard_fanout() {
    let (addr, db) = spawn_default().await;
    let mut c = ws_connect(addr).await;

    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"sensors.#"})).await;
    // Explicit ack (acks_subscribe:true): the sub id echoes the request id.
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"subscribed","sub":"1"}),
        "subscribe must be acked with the request id as sub id"
    );

    // The ack means the bus subscription is registered, so a fan-out reaches us
    // — tagged with the concrete topic the wildcard matched.
    inject(&db, "sensors.temp.vienna", json!(22.5));
    let ev = ws_recv_tag(&mut c, "event").await;
    assert_eq!(ev["sub"], "1");
    assert_eq!(ev["topic"], "sensors.temp.vienna");
    assert_eq!(ev["data"], json!(22.5));
}

#[tokio::test]
async fn server_two_subscriptions_and_unsubscribe() {
    let (addr, db) = spawn_default().await;
    let mut c = ws_connect(addr).await;

    // Two patterns are two `sub` frames (the multi-topic Subscribe is gone).
    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"a"})).await;
    ws_send(&mut c, json!({"t":"sub","id":2,"topic":"b"})).await;
    let mut acked = vec![
        ws_recv_tag(&mut c, "subscribed").await["sub"]
            .as_str()
            .unwrap()
            .to_string(),
        ws_recv_tag(&mut c, "subscribed").await["sub"]
            .as_str()
            .unwrap()
            .to_string(),
    ];
    acked.sort();
    assert_eq!(acked, vec!["1".to_string(), "2".to_string()]);

    inject(&db, "a", json!(1));
    let ev = ws_recv_tag(&mut c, "event").await;
    assert_eq!(ev["sub"], "1");
    assert_eq!(ev["topic"], "a");

    // Unsubscribe "a" by its sub id; a later "a" must not arrive, but "b" does.
    ws_send(&mut c, json!({"t":"unsub","sub":"1"})).await;
    tokio::time::sleep(Duration::from_millis(100)).await; // let the unsub settle
    inject(&db, "a", json!(2));
    inject(&db, "b", json!(3));
    let ev = ws_recv_tag(&mut c, "event").await;
    assert_eq!(ev["sub"], "2", "only 'b' should arrive");
    assert_eq!(ev["topic"], "b");
}

#[tokio::test]
async fn server_late_join_snapshot() {
    let (addr, db) = spawn_default().await;
    // Produce the value first so the late-join cache holds it, then subscribe.
    inject(&db, "sensors.temp", json!(99));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c = ws_connect(addr).await;
    ws_send(&mut c, json!({"t":"sub","id":4,"topic":"sensors.temp"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"subscribed","sub":"4"}));
    // The snapshot rides between the ack and the first event, tagged with the
    // subscription that triggered it and numbered in that subscription's `seq`
    // space (events continue after the burst) so a dropped snapshot shows up as
    // a gap rather than vanishing. `last` closes the burst — here the only
    // snapshot is also the final one.
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"snap","sub":"4","seq":1,"last":true,"topic":"sensors.temp","data":99})
    );
}

#[tokio::test]
async fn server_wildcard_late_join_snapshots_per_match() {
    let (addr, db) = spawn_default().await;
    inject(&db, "sensors.temp", json!(1));
    inject(&db, "sensors.humidity", json!(2));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c = ws_connect(addr).await;
    ws_send(&mut c, json!({"t":"sub","id":9,"topic":"sensors.#"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"subscribed","sub":"9"}));
    // One snapshot per cached record under the pattern (order is map order),
    // numbered 1..=N with only the final one flagged `last` — that flag is what
    // lets a client close out the burst without waiting for a live event.
    let mut snaps = Vec::new();
    let mut flags = Vec::new();
    for i in 1..=2 {
        let s = ws_recv_tag(&mut c, "snap").await;
        assert_eq!(s["sub"], "9");
        assert_eq!(s["seq"], i, "snapshots are numbered in burst order");
        snaps.push(s["topic"].as_str().unwrap().to_string());
        flags.push(s["last"].as_bool().unwrap_or(false));
    }
    snaps.sort();
    assert_eq!(snaps, vec!["sensors.humidity", "sensors.temp"]);
    assert_eq!(
        flags,
        vec![false, true],
        "only the burst's final snapshot carries `last`"
    );
}

#[tokio::test]
async fn server_query_and_record_list() {
    let addr = free_addr();
    let mut ws = WebSocketConnector::new().with_query_handler(OneRecordQuery);
    ws.register::<Temp>();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"));
    sb.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws://temp")
            .with_serializer(|_ctx, t: &Temp| Ok(serde_json::to_vec(t).unwrap()))
            .finish();
    });
    let (db, runner) = sb.build().await.expect("build db");
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;

    let mut c = ws_connect(addr).await;

    // record.query rides a plain AimX request; the reply carries the shared
    // `{records, total}` shape.
    ws_send(
        &mut c,
        json!({"t":"req","id":10,"method":"record.query","params":{"name":"#"}}),
    )
    .await;
    let reply = ws_recv_tag(&mut c, "reply").await;
    assert_eq!(reply["id"], 10);
    assert_eq!(reply["ok"]["total"], 1);
    assert_eq!(
        reply["ok"]["records"],
        json!([{"topic":"temp","payload":21.0,"ts":7}])
    );

    // record.list returns core's shared `RecordMetadata` rows — the same shape
    // every transport serves — with the schema name the connector resolved
    // stamped in.
    ws_send(
        &mut c,
        json!({"t":"req","id":11,"method":"record.list","params":null}),
    )
    .await;
    let reply = ws_recv_tag(&mut c, "reply").await;
    assert_eq!(reply["id"], 11);
    let rows = reply["ok"].as_array().expect("record.list array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["record_key"], "temp");
    assert_eq!(row["entity"], "temp");
    assert_eq!(row["buffer_type"], "single_latest");
    assert_eq!(row["schema_type"], "temperature");

    // The row is byte-for-byte what core produces for the same record, aside
    // from the schema name only the connector can resolve.
    let core_rows = serde_json::to_value(db.list_records()).unwrap();
    let mut ws_row = row.clone();
    ws_row.as_object_mut().unwrap().remove("schema_type");
    assert_eq!(ws_row, core_rows[0]);
}

#[tokio::test]
async fn server_query_without_handler_is_not_found() {
    let (addr, _db) = spawn_default().await;
    let mut c = ws_connect(addr).await;
    // No custom handler and no with_persistence → not_found (3-code vocabulary).
    ws_send(
        &mut c,
        json!({"t":"req","id":3,"method":"record.query","params":{"name":"*"}}),
    )
    .await;
    assert_eq!(
        ws_recv_tag(&mut c, "reply").await,
        json!({"t":"reply","id":3,"err":"not_found"})
    );
}

/// Regression (design 049 §2): `record.list` / `record.query` consulted nothing,
/// so a client with *empty* grants could enumerate core's whole database and
/// read whatever history `with_persistence` left in Extensions. Same server,
/// same database, two clients — the grant is the only difference.
#[tokio::test]
async fn record_list_and_query_answer_to_the_client_grants() {
    let addr = free_addr();
    let mut ws = WebSocketConnector::new().with_auth(GrantByQuery);
    ws.register::<Temp>();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"));

    // History on the *database*, as `with_persistence` registers it. No
    // `with_query_handler` here, so this exercises the Extensions fallback.
    let handler: QueryHandlerFn = Box::new(|_params| {
        Box::pin(async {
            Ok(json!({"records": [{"topic":"ledger","payload":42,"ts":1}], "total": 1}))
        })
    });
    sb.extensions_mut().insert(handler);

    sb.configure::<Temp>("temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws://temp")
            .with_serializer(|_ctx, t: &Temp| Ok(serde_json::to_vec(t).unwrap()))
            .finish();
    });
    sb.configure::<Ledger>("ledger", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = sb.build().await.expect("build db");
    assert_eq!(db.list_records().len(), 2, "two records to discriminate on");
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;

    // ── No grants ────────────────────────────────────────────────────
    let mut c = ws_connect(addr).await;

    // The grants really are empty (the premise the rest of the test rests on).
    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"temp"})).await;
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"reply","id":1,"err":"denied"})
    );

    // `denied` either way — configured handler or not — so it leaks nothing.
    ws_send(
        &mut c,
        json!({"t":"req","id":2,"method":"record.query","params":{}}),
    )
    .await;
    assert_eq!(
        ws_recv_tag(&mut c, "reply").await,
        json!({"t":"reply","id":2,"err":"denied"}),
        "the database's persistence handler must not be reachable ungranted"
    );

    // Nothing granted, nothing listed — the call still succeeds.
    ws_send(
        &mut c,
        json!({"t":"req","id":3,"method":"record.list","params":null}),
    )
    .await;
    let reply = ws_recv_tag(&mut c, "reply").await;
    assert_eq!(
        reply["ok"],
        json!([]),
        "an ungranted client must not enumerate the database"
    );

    // ── Full grants, same server ─────────────────────────────────────
    let mut a = ws_connect_with(addr, "&grant=all").await;

    // The Extensions fallback still serves history — it is gated, not removed.
    ws_send(
        &mut a,
        json!({"t":"req","id":4,"method":"record.query","params":{"name":"#"}}),
    )
    .await;
    let reply = ws_recv_tag(&mut a, "reply").await;
    assert_eq!(reply["ok"]["total"], 1);
    assert_eq!(
        reply["ok"]["records"],
        json!([{"topic":"ledger","payload":42,"ts":1}])
    );

    // …and the full database is enumerable, unregistered `Ledger` included.
    ws_send(
        &mut a,
        json!({"t":"req","id":5,"method":"record.list","params":null}),
    )
    .await;
    let reply = ws_recv_tag(&mut a, "reply").await;
    let rows = reply["ok"].as_array().expect("record.list array");
    let mut keys: Vec<&str> = rows
        .iter()
        .map(|r| r["record_key"].as_str().unwrap())
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["ledger", "temp"]);
    let temp = rows.iter().find(|r| r["record_key"] == "temp").unwrap();
    assert_eq!(temp["schema_type"], "temperature");
}

#[tokio::test]
async fn server_ping_pong() {
    let (addr, _db) = spawn_default().await;
    let mut c = ws_connect(addr).await;
    ws_send(&mut c, json!({"t":"ping"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"pong"}));
}

#[tokio::test]
async fn server_rejects_unauthenticated_upgrade() {
    let (addr, _db) = spawn(WebSocketConnector::new().with_auth(DenyAuth)).await;
    // A compatible version so the request reaches auth; the upgrade must then be
    // refused with HTTP 401 → the WS handshake fails.
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    let result = tokio_tungstenite::connect_async(url).await;
    assert!(result.is_err(), "auth-rejected upgrade should not connect");
}

#[tokio::test]
async fn server_rejects_incompatible_protocol_version() {
    let (addr, _db) = spawn_default().await;
    // A pre-3.x client (or one omitting `?v`) is refused at the upgrade (426),
    // before the socket opens — it never reaches the AimX frame loop.
    let stale = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?v=2.0")).await;
    assert!(stale.is_err(), "incompatible version must not upgrade");
    let missing = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    assert!(
        missing.is_err(),
        "absent version must not upgrade (fail closed)"
    );
    // The current version still connects fine.
    let ok = tokio_tungstenite::connect_async(aimdb_core::remote::ws_url_with_version(&format!(
        "ws://{addr}/ws"
    )))
    .await;
    assert!(ok.is_ok(), "current version must upgrade");
}

/// The connection cap is enforced at the upgrade: once `with_max_clients` are
/// connected, further upgrades are refused with 503 before the socket opens, and
/// closing one frees its slot. A refused upgrade must not consume a slot itself.
#[tokio::test]
async fn server_rejects_upgrade_past_the_client_cap() {
    const LIMIT: usize = 3;
    let (addr, _db) = spawn(WebSocketConnector::new().with_max_clients(LIMIT)).await;

    // Hold the sockets — dropping one would free its slot mid-test.
    let mut live: Vec<WsClient> = Vec::new();
    for _ in 0..LIMIT {
        live.push(ws_connect(addr).await);
    }
    assert_eq!(
        health_clients(addr).await,
        LIMIT,
        "a slot is claimed at the upgrade, so the count is exact once the 101 lands"
    );

    let err = tokio_tungstenite::connect_async(aimdb_core::remote::ws_url_with_version(&format!(
        "ws://{addr}/ws"
    )))
    .await
    .expect_err("the client past the cap must be refused");
    let Error::Http(resp) = err else {
        panic!("expected an HTTP rejection, got {err:?}");
    };
    // Asserting the status, not just the failure: the same dial would also fail
    // with 426 or 401, and those would mean the cap never ran.
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "over-capacity upgrade must be refused with 503, body: {:?}",
        resp.body().as_deref().map(String::from_utf8_lossy),
    );
    assert_eq!(
        health_clients(addr).await,
        LIMIT,
        "a refused upgrade must not consume a slot"
    );

    // Closing one connection frees its slot for the next client.
    drop(live.pop().expect("a live client"));
    assert_eq!(
        health_clients_reaches(addr, LIMIT - 1).await,
        LIMIT - 1,
        "a closed connection must release its slot"
    );
    live.push(ws_connect(addr).await);
}

/// The cap must hold when upgrades arrive *together*, not just one at a time.
///
/// A sequential test cannot catch this: on a current-thread runtime each dial is
/// awaited to completion, so the server's session task is always scheduled before
/// the next dial reads the count. Under concurrent dials every in-flight handler
/// observes the same value, so the admission check and the counter's increment
/// have to be one atomic step ([`ClientManager::try_connection_guard`]) — with a
/// plain load-then-increment they all wave each other through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_cap_holds_under_concurrent_upgrades() {
    const LIMIT: usize = 3;
    const DIALS: usize = 8;
    let (addr, _db) = spawn(WebSocketConnector::new().with_max_clients(LIMIT)).await;
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));

    // All dials in flight at once, so they race the admission gate.
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..DIALS {
        let url = url.clone();
        set.spawn(async move { tokio_tungstenite::connect_async(url).await });
    }

    // Hold the admitted sockets — dropping one would free its slot mid-assertion.
    let mut admitted: Vec<WsClient> = Vec::new();
    let mut refused = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined.expect("dial task panicked") {
            Ok((sock, _)) => admitted.push(sock),
            Err(Error::Http(resp)) => {
                assert_eq!(
                    resp.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "over-capacity dials must be refused with 503"
                );
                refused += 1;
            }
            Err(e) => panic!("unexpected dial failure: {e:?}"),
        }
    }

    // Exact, not `<=`: this also catches a gate that refuses while slots are free.
    assert_eq!(
        admitted.len(),
        LIMIT,
        "cap of {LIMIT} not held under concurrent dials"
    );
    assert_eq!(
        refused,
        DIALS - LIMIT,
        "every dial past the cap must be refused"
    );
    assert_eq!(health_clients(addr).await, LIMIT);
}

#[tokio::test]
async fn server_serves_protocol_version_over_http() {
    let (addr, _db) = spawn_default().await;

    // The 426 the upgrade gate returns is unreadable from a browser, so the
    // same version is served as plain JSON on a route `fetch` can reach.
    let (status, body) = http_get(addr, "/version").await;
    assert!(status.contains("200"), "unexpected status: {status}");
    let parsed: Value = serde_json::from_str(&body).expect("version body is JSON");
    assert_eq!(parsed["aimx"], aimdb_core::remote::PROTOCOL_VERSION);

    // A client that reads it and dials with it is accepted by the gate, which
    // is the whole point of publishing it.
    let dialed = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?v={}",
        parsed["aimx"].as_str().unwrap()
    ))
    .await;
    assert!(dialed.is_ok(), "the advertised version must upgrade");
}

#[tokio::test]
async fn server_survives_malformed_frame() {
    let (addr, db) = spawn_default().await;
    let mut c = ws_connect(addr).await;

    // Garbage that is not an AimX frame — the session must skip it, not die.
    c.send(Message::Text("{not valid".to_string().into()))
        .await
        .unwrap();
    // The connection is still usable afterwards.
    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"x"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"subscribed","sub":"1"}));
    inject(&db, "x", json!(1));
    let ev = ws_recv_tag(&mut c, "event").await;
    assert_eq!(ev["topic"], "x");
}

#[tokio::test]
async fn server_write_reaches_inbound_record() {
    let addr = free_addr();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(WebSocketConnector::new().bind(addr).path("/ws"));
    sb.configure::<Temp>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws://cfg")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Temp>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    let (db, runner) = sb.build().await.expect("build db");
    let db = Arc::new(db);
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;

    let mut c = ws_connect(addr).await;
    // Fire-and-forget write: the payload is the raw record value (no wrapper).
    ws_send(
        &mut c,
        json!({"t":"write","topic":"cfg","payload":{"c":7.5}}),
    )
    .await;
    // FIFO on the one connection: the pong proves the write frame was processed.
    ws_send(&mut c, json!({"t":"ping"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"pong"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(db.try_latest_as_json("cfg"), Some(json!({"c":7.5})));
}

// ── Client engine e2e (run_client + WsDialer over a real socket) ─────

#[tokio::test]
async fn client_engine_receives_broadcast_over_real_socket() {
    let (addr, db) = spawn_default().await;

    let config = ClientConfig {
        reconnect: false,
        ..ClientConfig::default()
    };
    let (handle, engine) = run_client(
        WsDialer::new(format!("ws://{addr}/ws")),
        AimxCodec,
        config,
        Arc::new(TokioAdapter),
    );
    let driver = tokio::spawn(engine);

    let mut stream = handle.subscribe("sensors.temp").unwrap();

    // Subscription registration is async; re-inject until the value arrives.
    let mut got = None;
    for _ in 0..100 {
        inject(&db, "sensors.temp", json!(42));
        if let Ok(Some(Ok(item))) = timeout(Duration::from_millis(20), stream.next()).await {
            got = Some(item);
            break;
        }
    }
    // The record value round-trips; the bus tags every event with its topic.
    let update = got.expect("a value");
    assert_eq!(&update.data[..], b"42");
    assert_eq!(update.topic.as_deref(), Some("sensors.temp"));

    drop(handle);
    drop(stream);
    let _ = driver.await;
}

// ── Concurrency / backpressure ───────────────────────────────────────

#[tokio::test]
async fn many_clients_fanout() {
    let (addr, db) = spawn_default().await;

    let mut clients = Vec::new();
    for _ in 0..20 {
        let mut c = ws_connect(addr).await;
        ws_send(&mut c, json!({"t":"sub","id":1,"topic":"evt.#"})).await;
        assert_eq!(ws_recv(&mut c).await["t"], "subscribed");
        clients.push(c);
    }

    // One broadcast reaches all 20.
    inject(&db, "evt.x", json!(1));
    for c in &mut clients {
        let ev = ws_recv_tag(c, "event").await;
        assert_eq!(ev["topic"], "evt.x");
    }
}

#[tokio::test]
async fn stalled_client_does_not_block_a_healthy_one() {
    let (addr, db) = spawn_default().await;

    // Stalled: subscribes but never reads — its socket backpressures.
    let mut stalled = ws_connect(addr).await;
    ws_send(&mut stalled, json!({"t":"sub","id":1,"topic":"x"})).await;

    let mut healthy = ws_connect(addr).await;
    ws_send(&mut healthy, json!({"t":"sub","id":1,"topic":"x"})).await;
    assert_eq!(ws_recv(&mut healthy).await["t"], "subscribed");
    tokio::time::sleep(Duration::from_millis(100)).await; // let the stalled sub register

    // Flood well past the bounded funnel (256). This also overruns the injector
    // ring, so the outbound `pump_sink` consumer lags — it must skip the gap and
    // keep publishing (not die), while the stalled client's pump drops on overflow
    // and the healthy client keeps up.
    for i in 0..2000u32 {
        inject(&db, "x", json!(i));
    }

    let ev = ws_recv_tag(&mut healthy, "event").await;
    assert_eq!(
        ev["topic"], "x",
        "healthy client must keep receiving past a stalled peer"
    );
    let _ = stalled.close(None).await;
}

// ── Golden wire frames (locks the exact on-the-wire JSON shape) ──────

#[tokio::test]
async fn golden_wire_frames() {
    let (addr, db) = spawn_default().await;
    inject(&db, "t", json!(5)); // seed the late-join cache
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c = ws_connect(addr).await;
    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"t"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"subscribed","sub":"1"}));
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"snap","sub":"1","seq":1,"last":true,"topic":"t","data":5})
    );

    // The event continues the snapshot burst's `seq` (one snapshot, so `seq:2`)
    // — a single counter over the whole subscription, so loss anywhere in it is
    // visible as a gap.
    inject(&db, "t", json!(42));
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"event","sub":"1","seq":2,"topic":"t","data":42})
    );

    ws_send(&mut c, json!({"t":"ping"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"pong"}));
}

// ── Async authorization over a real socket ───────────────────────────

#[tokio::test]
async fn async_authorize_subscribe_gates_despite_allow_all_permissions() {
    let (addr, db) = spawn(WebSocketConnector::new().with_auth(AsyncTopicAuth)).await;
    let mut c = ws_connect(addr).await;

    // Denied topic: permissions are allow-all, but the *async* hook says no.
    // The refusal is a `reply` carrying the subscribe id + the 3-code error.
    ws_send(&mut c, json!({"t":"sub","id":1,"topic":"secret.x"})).await;
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"reply","id":1,"err":"denied"})
    );

    // An allowed topic still works end-to-end.
    ws_send(&mut c, json!({"t":"sub","id":2,"topic":"public.x"})).await;
    assert_eq!(ws_recv(&mut c).await, json!({"t":"subscribed","sub":"2"}));
    inject(&db, "public.x", json!(1));
    let ev = ws_recv_tag(&mut c, "event").await;
    assert_eq!(ev["topic"], "public.x");
}
