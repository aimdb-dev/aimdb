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
//! snapshot, query, list — design 045 §2.1).
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
use tokio_tungstenite::tungstenite::Message;

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
    tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("connect")
        .0
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
    // subscription that triggered it.
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"snap","sub":"4","topic":"sensors.temp","data":99})
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
    // One snapshot per cached record under the pattern (order is map order).
    let mut snaps = Vec::new();
    for _ in 0..2 {
        let s = ws_recv_tag(&mut c, "snap").await;
        assert_eq!(s["sub"], "9");
        snaps.push(s["topic"].as_str().unwrap().to_string());
    }
    snaps.sort();
    assert_eq!(snaps, vec!["sensors.humidity", "sensors.temp"]);
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
    let (_db, runner) = sb.build().await.expect("build db");
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

    // record.list returns `{name, schema_type, entity}` rows.
    ws_send(
        &mut c,
        json!({"t":"req","id":11,"method":"record.list","params":null}),
    )
    .await;
    let reply = ws_recv_tag(&mut c, "reply").await;
    assert_eq!(reply["id"], 11);
    assert_eq!(
        reply["ok"],
        json!([{"name":"temp","schema_type":"temperature","entity":"temp"}])
    );
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
    // The upgrade must be refused with HTTP 401 → the WS handshake fails.
    let result = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    assert!(result.is_err(), "auth-rejected upgrade should not connect");
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
        json!({"t":"snap","sub":"1","topic":"t","data":5})
    );

    inject(&db, "t", json!(42));
    assert_eq!(
        ws_recv(&mut c).await,
        json!({"t":"event","sub":"1","seq":1,"topic":"t","data":42})
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
