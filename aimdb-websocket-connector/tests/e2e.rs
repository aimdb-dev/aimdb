//! Real-socket integration tests for the WebSocket connector — black-box.
//!
//! These drive the connector through its **public API only**: a server `AimDb`
//! stood up with [`WebSocketConnector`] over a real TCP socket, talked to by a
//! raw `tokio-tungstenite` client (or the public `run_client` + [`WsDialer`]
//! engine). Server→client data is pushed by *producing a record* — an "injector"
//! record whose dynamic topic + raw serializer let a test broadcast an arbitrary
//! `(topic, payload)` through the real `pump_sink` → bus → session path.
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
use aimdb_core::session::{run_client, ClientConfig};
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_data_contracts::{SchemaType, Streamable};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::codec::WsCodec;
use aimdb_websocket_connector::transport::WsDialer;
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, ClientInfo, ClientMessage, ErrorCode, Permissions,
    QueryFuture, QueryHandler, QueryRecord, ServerMessage, WebSocketConnector,
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

// ── A registered Streamable type (for the `list_topics` schema name) ──

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
async fn spawn(ws: WebSocketConnector) -> (SocketAddr, Arc<AimDb<TokioAdapter>>) {
    let addr = free_addr();
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(ws.bind(addr).path("/ws"));
    sb.configure::<Inject>("inject", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 1024 })
            .with_remote_access()
            .link_to("ws://_") // overridden per-value by the topic provider
            .with_topic_provider(InjectTopic)
            .with_serializer_raw(|m: &Inject| {
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
async fn spawn_default() -> (SocketAddr, Arc<AimDb<TokioAdapter>>) {
    spawn(WebSocketConnector::new().with_late_join(true)).await
}

/// Push `payload` to subscribers of `topic` (one outbound record update).
fn inject(db: &AimDb<TokioAdapter>, topic: &str, payload: Value) {
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

async fn ws_send(c: &mut WsClient, m: ClientMessage) {
    c.send(Message::Text(serde_json::to_string(&m).unwrap().into()))
        .await
        .unwrap();
}

/// Read the next `ServerMessage`, with a timeout so a hang fails loudly.
async fn ws_recv(c: &mut WsClient) -> ServerMessage {
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

/// Like [`ws_recv`] but returns the raw JSON with `ts` normalized to `0`.
async fn recv_value(c: &mut WsClient) -> Value {
    loop {
        match timeout(Duration::from_secs(3), c.next())
            .await
            .expect("recv timed out")
        {
            Some(Ok(Message::Text(t))) => {
                let mut v: Value = serde_json::from_str(&t).unwrap();
                if let Some(ts) = v.get_mut("ts") {
                    *ts = json!(0);
                }
                return v;
            }
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

// ── Server e2e ───────────────────────────────────────────────────────

#[tokio::test]
async fn server_subscribe_ack_and_wildcard_fanout() {
    let (addr, db) = spawn_default().await;
    let mut c = ws_connect(addr).await;

    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["sensors/#".into()],
        },
    )
    .await;
    assert!(
        matches!(ws_recv(&mut c).await, ServerMessage::Subscribed { topics } if topics == vec!["sensors/#".to_string()])
    );

    // The ack means the bus subscription is registered, so a fan-out reaches us.
    inject(&db, "sensors/temp/vienna", json!(22.5));
    match ws_recv(&mut c).await {
        ServerMessage::Data { topic, payload, .. } => {
            assert_eq!(topic, "sensors/temp/vienna");
            assert_eq!(payload, Some(json!(22.5)));
        }
        other => panic!("expected Data, got {other:?}"),
    }
}

#[tokio::test]
async fn server_multi_topic_subscribe_and_unsubscribe() {
    let (addr, db) = spawn_default().await;
    let mut c = ws_connect(addr).await;

    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["a".into(), "b".into()],
        },
    )
    .await;
    let mut acked = Vec::new();
    for _ in 0..2 {
        if let ServerMessage::Subscribed { topics } = ws_recv(&mut c).await {
            acked.extend(topics);
        }
    }
    acked.sort();
    assert_eq!(acked, vec!["a".to_string(), "b".to_string()]);

    inject(&db, "a", json!(1));
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "a"));

    // Unsubscribe "a"; a later "a" must not arrive, but "b" still does.
    ws_send(
        &mut c,
        ClientMessage::Unsubscribe {
            topics: vec!["a".into()],
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await; // let the unsub settle
    inject(&db, "a", json!(2));
    inject(&db, "b", json!(3));
    match ws_recv(&mut c).await {
        ServerMessage::Data { topic, .. } => assert_eq!(topic, "b", "only 'b' should arrive"),
        other => panic!("expected Data on b, got {other:?}"),
    }
}

#[tokio::test]
async fn server_late_join_snapshot() {
    let (addr, db) = spawn_default().await;
    // Produce the value first so the late-join cache holds it, then subscribe.
    inject(&db, "sensors/temp", json!(99));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c = ws_connect(addr).await;
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["sensors/temp".into()],
        },
    )
    .await;
    assert!(matches!(
        ws_recv(&mut c).await,
        ServerMessage::Subscribed { .. }
    ));
    match ws_recv(&mut c).await {
        ServerMessage::Snapshot { topic, payload } => {
            assert_eq!(topic, "sensors/temp");
            assert_eq!(payload, Some(json!(99)));
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn server_query_and_list_topics() {
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
            .with_serializer_raw(|t: &Temp| Ok(serde_json::to_vec(t).unwrap()))
            .finish();
    });
    let (_db, runner) = sb.build().await.expect("build db");
    tokio::spawn(runner.run());
    wait_for_listen(addr).await;

    let mut c = ws_connect(addr).await;

    ws_send(
        &mut c,
        ClientMessage::Query {
            id: "q1".into(),
            pattern: "#".into(),
            from: None,
            to: None,
            limit: None,
        },
    )
    .await;
    match ws_recv(&mut c).await {
        ServerMessage::QueryResult { id, records, total } => {
            assert_eq!(id, "q1"); // the String id round-trips
            assert_eq!(total, 1);
            assert_eq!(records.len(), 1);
        }
        other => panic!("expected QueryResult, got {other:?}"),
    }

    ws_send(&mut c, ClientMessage::ListTopics { id: "l1".into() }).await;
    match ws_recv(&mut c).await {
        ServerMessage::TopicList { id, topics } => {
            assert_eq!(id, "l1");
            assert_eq!(topics.len(), 1);
            assert_eq!(topics[0].name, "temp");
            assert_eq!(topics[0].schema_type.as_deref(), Some("temperature"));
        }
        other => panic!("expected TopicList, got {other:?}"),
    }
}

#[tokio::test]
async fn server_ping_pong() {
    let (addr, _db) = spawn_default().await;
    let mut c = ws_connect(addr).await;
    ws_send(&mut c, ClientMessage::Ping).await;
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Pong));
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

    // Garbage that is not a ClientMessage — the session must skip it, not die.
    c.send(Message::Text("{not valid".to_string().into()))
        .await
        .unwrap();
    // The connection is still usable afterwards.
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["x".into()],
        },
    )
    .await;
    assert!(matches!(
        ws_recv(&mut c).await,
        ServerMessage::Subscribed { .. }
    ));
    inject(&db, "x", json!(1));
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "x"));
}

// ── Client engine e2e (run_client + WsDialer over a real socket) ─────

#[tokio::test]
async fn client_engine_receives_broadcast_over_real_socket() {
    let (addr, db) = spawn_default().await;

    let config = ClientConfig {
        topic_routed_subs: true,
        reconnect: false,
        ..ClientConfig::default()
    };
    let (handle, engine) = run_client(
        WsDialer::new(format!("ws://{addr}/ws")),
        WsCodec::new(),
        config,
        Arc::new(TokioAdapter),
    );
    let driver = tokio::spawn(engine);

    let mut stream = handle.subscribe("sensors/temp").unwrap();

    // Subscription registration is async; re-inject until the value arrives.
    let mut got = None;
    for _ in 0..100 {
        inject(&db, "sensors/temp", json!(42));
        if let Ok(Some(item)) = timeout(Duration::from_millis(20), stream.next()).await {
            got = Some(item);
            break;
        }
    }
    // The record value round-trips: Data{payload:42} → engine stream yields b"42".
    assert_eq!(&got.expect("a value")[..], b"42");

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
        ws_send(
            &mut c,
            ClientMessage::Subscribe {
                topics: vec!["evt/#".into()],
            },
        )
        .await;
        assert!(matches!(
            ws_recv(&mut c).await,
            ServerMessage::Subscribed { .. }
        ));
        clients.push(c);
    }

    // One broadcast reaches all 20.
    inject(&db, "evt/x", json!(1));
    for c in &mut clients {
        assert!(matches!(ws_recv(c).await, ServerMessage::Data { topic, .. } if topic == "evt/x"));
    }
}

#[tokio::test]
async fn stalled_client_does_not_block_a_healthy_one() {
    let (addr, db) = spawn_default().await;

    // Stalled: subscribes but never reads — its socket backpressures.
    let mut stalled = ws_connect(addr).await;
    ws_send(
        &mut stalled,
        ClientMessage::Subscribe {
            topics: vec!["x".into()],
        },
    )
    .await;

    let mut healthy = ws_connect(addr).await;
    ws_send(
        &mut healthy,
        ClientMessage::Subscribe {
            topics: vec!["x".into()],
        },
    )
    .await;
    assert!(matches!(
        ws_recv(&mut healthy).await,
        ServerMessage::Subscribed { .. }
    ));
    tokio::time::sleep(Duration::from_millis(100)).await; // let the stalled sub register

    // Flood well past the bounded funnel (256). This also overruns the injector
    // ring, so the outbound `pump_sink` consumer lags — it must skip the gap and
    // keep publishing (not die), while the stalled client's pump drops on overflow
    // and the healthy client keeps up.
    for i in 0..2000u32 {
        inject(&db, "x", json!(i));
    }

    assert!(
        matches!(ws_recv(&mut healthy).await, ServerMessage::Data { topic, .. } if topic == "x"),
        "healthy client must keep receiving past a stalled peer",
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
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["t".into()],
        },
    )
    .await;
    assert_eq!(
        recv_value(&mut c).await,
        json!({"type": "subscribed", "topics": ["t"]})
    );
    assert_eq!(
        recv_value(&mut c).await,
        json!({"type": "snapshot", "topic": "t", "payload": 5})
    );

    inject(&db, "t", json!(42));
    assert_eq!(
        recv_value(&mut c).await,
        json!({"type": "data", "topic": "t", "payload": 42, "ts": 0})
    );

    ws_send(&mut c, ClientMessage::Ping).await;
    assert_eq!(recv_value(&mut c).await, json!({"type": "pong"}));
}

// ── Async authorization over a real socket ───────────────────────────

#[tokio::test]
async fn async_authorize_subscribe_gates_despite_allow_all_permissions() {
    let (addr, db) = spawn(WebSocketConnector::new().with_auth(AsyncTopicAuth)).await;
    let mut c = ws_connect(addr).await;

    // Denied topic: permissions are allow-all, but the *async* hook says no.
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["secret/x".into()],
        },
    )
    .await;
    match ws_recv(&mut c).await {
        ServerMessage::Error { code, .. } => assert!(matches!(code, ErrorCode::Forbidden)),
        other => panic!("expected Forbidden Error for denied subscribe, got {other:?}"),
    }

    // An allowed topic still works end-to-end.
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["public/x".into()],
        },
    )
    .await;
    assert!(matches!(
        ws_recv(&mut c).await,
        ServerMessage::Subscribed { .. }
    ));
    inject(&db, "public/x", json!(1));
    assert!(
        matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "public/x")
    );
}
