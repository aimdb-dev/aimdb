//! Layer 1 real-socket integration tests (doc 039-validation).
//!
//! These run the **real** stack over a real TCP socket: a tungstenite client (or
//! the WS client engine) ↔ an axum server driving `run_session` + `WsCodec` +
//! `WsDispatch` + the `ClientManager` bus. Gated on `test` + both features so the
//! per-connection path that unit tests can only mock is actually exercised.

use core::future::Future;
use core::pin::Pin;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aimdb_core::router::RouterBuilder;
use aimdb_core::session::{run_client, ClientConfig};
use aimdb_core::Dispatch;
use aimdb_ws_protocol::{QueryRecord, TopicInfo};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{AuthError, AuthHandler, AuthRequest, NoAuth, Permissions};
use crate::client_manager::ClientManager;
use crate::codec::WsCodec;
use crate::dispatch::WsDispatch;
use crate::protocol::{ClientMessage, ServerMessage};
use crate::server::{build_app, ServerState};
use crate::session::{NoQuery, NoSnapshot, QueryFuture, QueryHandler, SnapshotProvider};
use crate::transport::WsDialer;

// ── Test fixtures ────────────────────────────────────────────────────

struct OneSnap(&'static str, &'static [u8]);
impl SnapshotProvider for OneSnap {
    fn snapshot(&self, topic: &str) -> Option<Vec<u8>> {
        (topic == self.0).then(|| self.1.to_vec())
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
                    payload: serde_json::json!(21.0),
                    ts: 7,
                }],
                1,
            ))
        })
    }
}

struct DenyAuth;
impl AuthHandler for DenyAuth {
    fn authenticate<'a>(
        &'a self,
        _request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        Box::pin(async { Err(AuthError::new("denied")) })
    }
}

/// Allows the connection with **allow-all** permissions, but asynchronously
/// **denies** subscribing to `secret/*` via `authorize_subscribe`. If the engine
/// only consulted the static permission set (the pre-fix sync path), `secret`
/// would be allowed — so this fixture proves the async hook actually gates (#3).
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
        _client: &'a crate::auth::ClientInfo,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let denied = topic.starts_with("secret");
        // Simulate an async ACL lookup.
        Box::pin(async move {
            tokio::task::yield_now().await;
            !denied
        })
    }
}

/// Knobs for the spawned server; defaults give an allow-all, no-snapshot server.
struct Opts {
    snapshot: Arc<dyn SnapshotProvider>,
    query: Arc<dyn QueryHandler>,
    known_topics: Vec<TopicInfo>,
    auth: Arc<dyn AuthHandler>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            snapshot: Arc::new(NoSnapshot),
            query: Arc::new(NoQuery),
            known_topics: Vec::new(),
            auth: Arc::new(NoAuth),
        }
    }
}

/// Bring up the real axum server on an ephemeral port; return its address and the
/// shared bus (so the test can `broadcast`, simulating an outbound record update).
async fn spawn(opts: Opts) -> (SocketAddr, ClientManager) {
    let client_mgr = ClientManager::new(false);
    let dispatch: Arc<dyn Dispatch> = Arc::new(WsDispatch {
        client_mgr: client_mgr.clone(),
        snapshot_provider: opts.snapshot,
        query_handler: opts.query,
        router: Arc::new(RouterBuilder::from_routes(Vec::new()).build()),
        known_topics: Arc::new(opts.known_topics),
        auth: opts.auth.clone(),
        late_join: true,
        runtime_ctx: None,
    });
    let state = ServerState {
        dispatch,
        auth: opts.auth,
        client_mgr: client_mgr.clone(),
        auto_subscribe: Arc::new(Vec::new()),
        max_subs_per_connection: 64,
        started_at: Instant::now(),
    };
    let app = build_app("/ws", state, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (addr, client_mgr)
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
        match tokio::time::timeout(Duration::from_secs(3), c.next())
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

// ── 1.1 Server e2e ───────────────────────────────────────────────────

#[tokio::test]
async fn server_subscribe_ack_and_wildcard_fanout() {
    let (addr, bus) = spawn(Opts::default()).await;
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

    // An outbound record update fans out as a Data frame with the *real* topic.
    bus.broadcast("sensors/temp/vienna", b"22.5").await;
    match ws_recv(&mut c).await {
        ServerMessage::Data { topic, payload, .. } => {
            assert_eq!(topic, "sensors/temp/vienna");
            assert_eq!(payload, Some(serde_json::json!(22.5)));
        }
        other => panic!("expected Data, got {other:?}"),
    }
}

#[tokio::test]
async fn server_multi_topic_subscribe_and_unsubscribe() {
    let (addr, bus) = spawn(Opts::default()).await;
    let mut c = ws_connect(addr).await;

    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["a".into(), "b".into()],
        },
    )
    .await;
    // Per-topic acks (the documented wire nuance).
    let mut acked = Vec::new();
    for _ in 0..2 {
        if let ServerMessage::Subscribed { topics } = ws_recv(&mut c).await {
            acked.extend(topics);
        }
    }
    acked.sort();
    assert_eq!(acked, vec!["a".to_string(), "b".to_string()]);

    bus.broadcast("a", b"1").await;
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "a"));

    // Unsubscribe from "a"; a later broadcast to "a" must not arrive, but "b" still does.
    ws_send(
        &mut c,
        ClientMessage::Unsubscribe {
            topics: vec!["a".into()],
        },
    )
    .await;
    // Give the engine a moment to process the unsubscribe.
    tokio::time::sleep(Duration::from_millis(100)).await;
    bus.broadcast("a", b"2").await;
    bus.broadcast("b", b"3").await;
    match ws_recv(&mut c).await {
        ServerMessage::Data { topic, .. } => assert_eq!(topic, "b", "only 'b' should arrive"),
        other => panic!("expected Data on b, got {other:?}"),
    }
}

#[tokio::test]
async fn server_late_join_snapshot() {
    let (addr, _bus) = spawn(Opts {
        snapshot: Arc::new(OneSnap("sensors/temp", b"99")),
        ..Opts::default()
    })
    .await;
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
            assert_eq!(payload, Some(serde_json::json!(99)));
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn server_query_and_list_topics() {
    let (addr, _bus) = spawn(Opts {
        query: Arc::new(OneRecordQuery),
        known_topics: vec![TopicInfo {
            name: "temp".into(),
            schema_type: Some("temperature".into()),
            entity: None,
        }],
        ..Opts::default()
    })
    .await;
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
        }
        other => panic!("expected TopicList, got {other:?}"),
    }
}

#[tokio::test]
async fn server_ping_pong() {
    let (addr, _bus) = spawn(Opts::default()).await;
    let mut c = ws_connect(addr).await;
    ws_send(&mut c, ClientMessage::Ping).await;
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Pong));
}

#[tokio::test]
async fn server_rejects_unauthenticated_upgrade() {
    let (addr, _bus) = spawn(Opts {
        auth: Arc::new(DenyAuth),
        ..Opts::default()
    })
    .await;
    // The upgrade must be refused with HTTP 401 → the WS handshake fails.
    let result = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    assert!(result.is_err(), "auth-rejected upgrade should not connect");
}

#[tokio::test]
async fn server_survives_malformed_frame() {
    let (addr, bus) = spawn(Opts::default()).await;
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
    bus.broadcast("x", b"1").await;
    assert!(matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "x"));
}

// ── 1.2 Client engine e2e (run_client + WsDialer over a real socket) ──

#[tokio::test]
async fn client_engine_receives_broadcast_over_real_socket() {
    let (addr, bus) = spawn(Opts::default()).await;

    let config = ClientConfig {
        topic_routed_subs: true,
        reconnect: false,
        ..ClientConfig::default()
    };
    let (handle, engine) = run_client(
        WsDialer::new(format!("ws://{addr}/ws")),
        WsCodec::new(),
        config,
    );
    let driver = tokio::spawn(engine);

    // Subscribe through the client engine; the dialer opens a real WebSocket.
    let mut stream = handle.subscribe("sensors/temp").unwrap();

    // Wait for the subscription to register on the server bus, then broadcast.
    for _ in 0..50 {
        if bus.subscription_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        bus.subscription_count(),
        1,
        "client subscribe should reach the server"
    );

    bus.broadcast("sensors/temp", b"42").await;

    let item = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("stream timed out")
        .expect("a value");
    // The record value round-trips: Data{payload:42} → engine stream yields b"42".
    assert_eq!(&item[..], b"42");

    drop(handle);
    drop(stream);
    let _ = driver.await;
}

// ── Layer 4 — concurrency / resource cleanup / backpressure ──────────

/// Poll `pred` until true or fail loudly.
async fn wait_until(mut pred: impl FnMut() -> bool, label: &str) {
    for _ in 0..300 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {label}");
}

#[tokio::test]
async fn many_clients_fanout_and_resource_cleanup() {
    let (addr, bus) = spawn(Opts::default()).await;

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
    wait_until(|| bus.subscription_count() == 20, "20 subscriptions").await;
    assert_eq!(bus.client_count(), 20);

    // One broadcast reaches all 20.
    bus.broadcast("evt/x", b"1").await;
    for c in &mut clients {
        assert!(matches!(ws_recv(c).await, ServerMessage::Data { topic, .. } if topic == "evt/x"));
    }

    // Disconnect everyone; the live-connection count returns to zero…
    for mut c in clients.drain(..) {
        let _ = c.close(None).await;
    }
    wait_until(|| bus.client_count() == 0, "0 connections").await;
    // …and a subsequent broadcast prunes the now-dead subscriptions.
    bus.broadcast("evt/x", b"2").await;
    wait_until(|| bus.subscription_count() == 0, "0 subscriptions").await;
}

#[tokio::test]
async fn stalled_client_does_not_block_a_healthy_one() {
    let (addr, bus) = spawn(Opts::default()).await;

    // Stalled: subscribes but never reads — its socket backpressures and its
    // bounded funnel fills, but it must not stall the server for others.
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
    wait_until(|| bus.subscription_count() == 2, "2 subscriptions").await;

    // Flood well past the bounded funnel (256). The stalled client's pump drops
    // on overflow rather than growing without bound; the healthy client keeps up.
    for i in 0..2000u32 {
        bus.broadcast("x", i.to_string().as_bytes()).await;
    }

    // The healthy client still receives data despite its stalled peer.
    assert!(
        matches!(ws_recv(&mut healthy).await, ServerMessage::Data { topic, .. } if topic == "x"),
        "healthy client must keep receiving past a stalled peer",
    );
    let _ = stalled.close(None).await;
}

// ── Layer 3.1 — golden wire frames (the masterplan wire-capture gate) ─

/// Receive the next text frame and parse it to a `Value`, normalizing the
/// time-dependent `ts` field to `0` so the shape can be asserted exactly.
async fn recv_value(c: &mut WsClient) -> serde_json::Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), c.next())
            .await
            .expect("recv timed out")
        {
            Some(Ok(Message::Text(t))) => {
                let mut v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if let Some(ts) = v.get_mut("ts") {
                    *ts = serde_json::json!(0);
                }
                return v;
            }
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

/// Locks the exact on-the-wire JSON shape (tag + field names + value types) the
/// server emits, so any accidental wire change is caught. Frame *shape* is what a
/// browser/wasm client depends on (key order is irrelevant to JSON).
#[tokio::test]
async fn golden_wire_frames() {
    use serde_json::json;
    let (addr, bus) = spawn(Opts {
        snapshot: Arc::new(OneSnap("t", b"5")),
        ..Opts::default()
    })
    .await;
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

    bus.broadcast("t", b"42").await;
    assert_eq!(
        recv_value(&mut c).await,
        json!({"type": "data", "topic": "t", "payload": 42, "ts": 0})
    );

    ws_send(&mut c, ClientMessage::Ping).await;
    assert_eq!(recv_value(&mut c).await, json!({"type": "pong"}));
}

// ── Layer 2.1 — the async-authz fix (#3) over a real socket ──────────

#[tokio::test]
async fn async_authorize_subscribe_gates_despite_allow_all_permissions() {
    let (addr, bus) = spawn(Opts {
        auth: Arc::new(AsyncTopicAuth),
        ..Opts::default()
    })
    .await;
    let mut c = ws_connect(addr).await;

    // Denied topic: permissions are allow-all, but the *async* hook says no.
    // Pre-fix (sync permission check) this would have been allowed → the test
    // would see a `Subscribed` ack instead of an error.
    ws_send(
        &mut c,
        ClientMessage::Subscribe {
            topics: vec!["secret/x".into()],
        },
    )
    .await;
    match ws_recv(&mut c).await {
        ServerMessage::Error { code, .. } => {
            assert!(matches!(code, crate::protocol::ErrorCode::Forbidden));
        }
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
    bus.broadcast("public/x", b"1").await;
    assert!(
        matches!(ws_recv(&mut c).await, ServerMessage::Data { topic, .. } if topic == "public/x")
    );
}
