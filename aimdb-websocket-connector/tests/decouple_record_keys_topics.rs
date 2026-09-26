//! `AimDb::collect_outbound_routes` over the `ws` scheme.
//! Record keys and ws topics are decoupled (#215).
//! Grants described record keys.
//!
//! Connectors call this during `build()` to spawn one publisher task per
//! configured `link_to("ws://…")`. The returned order must track record
//! registration order, since record ids index into it.
//!
//! Several behaviors tested: per-record gating, late-join snapshots,
//! `record.list` and `record.query`, zero-grant denial, and topic-based
//! write grants;

#![cfg(feature = "server")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::connector::TopicProvider;
use aimdb_core::remote::QueryHandlerFn;
use aimdb_core::{builder::AimDb, remote::QueryHandlerParams};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::{AuthError, AuthHandler, Permissions, WebSocketConnector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use core::future::Future;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Msg {
    v: u64,
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: SocketAddr) -> WsClient {
    ws_connect_with(addr, "").await
}

async fn ws_connect_with(addr: SocketAddr, extra: &str) -> WsClient {
    // Every real client declares its AimX version at the upgrade; go through the
    // shared helper so the tests exercise the exact URL the dialers produce.
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    tokio_tungstenite::connect_async(format!("{url}{extra}"))
        .await
        .expect("connect")
        .0
}

async fn try_ws_connect_with(addr: SocketAddr, extra: &str) -> Result<WsClient, Box<Error>> {
    // Every real client declares its AimX version at the upgrade; go through the
    // shared helper so the tests exercise the exact URL the dialers produce.
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    let (ws, _resp) = tokio_tungstenite::connect_async(format!("{url}{extra}")).await?;
    Ok(ws)
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

async fn try_ws_recv(c: &mut WsClient) -> Result<Value, String> {
    loop {
        match timeout(Duration::from_secs(3), c.next())
            .await
            .map_err(|_| "recv timed out".to_string())
        {
            Ok(Some(Ok(Message::Text(t)))) => return Ok(serde_json::from_str(&t).unwrap()),
            Ok(Some(Ok(Message::Binary(b)))) => return Ok(serde_json::from_slice(&b).unwrap()),
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => continue,
            Err(_) => Err("recv timed out".to_string()),
            other => Err(format!("unexpected ws frame: {other:?}")),
        }?
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

/// Similar to ws_recv_tag return Err instead of panic
async fn try_ws_recv_tag(c: &mut WsClient, tag: &str) -> Result<Value, String> {
    for _ in 0..50 {
        let v = try_ws_recv(c).await?;
        if v["t"] == tag {
            return Ok(v);
        }
    }
    Err("not found".to_string())
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

/// Grab a probably-free ephemeral port (the WS builder binds internally and does
/// not surface `:0`'s assigned port, so we pick one up front).
fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    a
}

#[tokio::test]
async fn collect_outbound_routes_preserves_record_order() {
    // A dummy address for server building only
    let addr = free_addr();

    let keys_topics = [
        ("garage", "peripheral"),
        ("house", "peripheral"),
        ("basement", "movement"),
    ];

    // Populate record id
    let mut record_keys: HashMap<&str, usize> = HashMap::new();

    let mut index = 0;
    keys_topics.iter().for_each(|(k, _v)| {
        record_keys.entry(*k).or_insert(index);
        index += 1;
    });

    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(WebSocketConnector::new().bind(addr).path("/ws"));

    keys_topics.iter().for_each(|(k, t)| {
        sb.configure::<Msg>(*k, |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .with_remote_access()
                .link_to(format!("ws://{}", t).as_str())
                .with_serializer(|_ctx, m: &Msg| Ok(serde_json::to_vec(m).expect("serialize")))
                .finish();
        });
    });

    let (server_db, _server_runner) = sb.build().await.expect("build server db");
    let outbound_routes = server_db.collect_outbound_routes("ws");
    assert_eq!(outbound_routes.len(), keys_topics.len());
    let outbound_iter = outbound_routes.into_iter();

    outbound_iter
        .into_iter()
        .zip(keys_topics.iter())
        .for_each(|(route, (k, t))| {
            assert_eq!(route.topic.as_str(), *t);

            let config_record_index: Vec<usize> = route
                .config
                .iter()
                .filter(|(k, _v)| k.as_str() == "record_index")
                .map(|(_k, v)| {
                    v.parse::<usize>()
                        .expect("failed to convert record_index to usize")
                })
                .collect();

            assert_eq!(
                config_record_index.len(),
                1,
                "config must have one tuple for record_index"
            );
            assert_eq!(
                config_record_index[0],
                *record_keys.get(*k).expect("key must exist")
            );
        });
}

//----------------- Custom AuthHandler and Permissions
struct FixedGrant(Permissions);
impl AuthHandler for FixedGrant {
    fn authenticate<'a>(
        &'a self,
        _request: &'a aimdb_websocket_connector::AuthRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        let perms = self.0.clone();
        Box::pin(async move { Ok(perms) })
    }
}

// Two or more distinct grants
struct DistinctGrant {
    pub public: Permissions,
    pub secret: Permissions,
}

impl AuthHandler for DistinctGrant {
    fn authenticate<'a>(
        &'a self,
        request: &'a aimdb_websocket_connector::AuthRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        // Grants are distinguished by a query param
        // clients must carry this param in request
        let perms_public = self.public.clone();
        let perms_secret = self.secret.clone();
        match request.query_params.get("data_type") {
            Some(s) if s.as_str() == "public" => Box::pin(async move { Ok(perms_public) }),
            Some(s) if s.as_str() == "secret" => Box::pin(async move { Ok(perms_secret) }),
            _ => Box::pin(async move {
                Err(AuthError {
                    message: "not authorized".to_string(),
                })
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Inject {
    topic: String,
    payload: Value,
}

struct InjectTopic;
impl TopicProvider<Inject> for InjectTopic {
    fn topic(&self, value: &Inject) -> Option<String> {
        Some(value.topic.clone())
    }
}

enum GrantType {
    Fixed,
    Distinct,
    Injected,
    WritePublic,
    OldRecord,
    PublicAndOld,
    None,
}

/// Different option for handle to test record.query
enum CustomQueryHandler {
    All,
    NoRecord,
    Malformed,
}

impl CustomQueryHandler {
    pub fn get_handler_fn(self) -> QueryHandlerFn {
        match self {
            Self::All => {
                let handler: QueryHandlerFn = Box::new(|p: QueryHandlerParams| {
                    Box::pin(async move {
                        // Tuple of key, value, timestamp
                        let all = [
                            ("public.ledger", 1, 1),
                            ("secret.sensor", 2, 2),
                            ("old.sensor", 3, 3),
                        ];
                        let records: Vec<_> = all
                            .iter()
                            .filter(|(key, _, _)| aimdb_core::topic_matches(&p.name, key))
                            .map(|(key, payload, ts)| json!({"topic": key, "payload": payload, "ts": ts}))
                            .collect();
                        let total = records.len();
                        Ok(json!({"records": records, "total": total}))
                    })
                });
                handler
            }
            Self::NoRecord => Box::new(|_| Box::pin(async move { Ok(json!({"total": 0})) })),
            Self::Malformed => {
                Box::new(|_| Box::pin(async move { Ok(json!({"records": "malformed"})) }))
            }
        }
    }
}

/// Test fixture for several testing scenarios
async fn test_fixture(
    addr: SocketAddr,
    grant_type: GrantType,
    query_handler: Option<CustomQueryHandler>,
) -> AimDb {
    // Different pair of record - topic
    let keys_topics = [
        ("public.ledger", "public_info"),
        ("public.data", "public_info"),
        ("sensor", "public_info"),
        ("secret.sensor", "secret_info"),
    ];

    // Different grant scenarios
    let mut sb = match grant_type {
        GrantType::Fixed => {
            // One fixed grant for all clients
            let perms = Permissions {
                read_patterns: vec!["public.#".to_string()],
                write_patterns: vec![],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::Distinct => {
            // Two type of grants for different type of clients
            let perms_public = Permissions {
                read_patterns: vec!["public.#".to_string()],
                write_patterns: vec![],
            };
            let perms_secret = Permissions {
                read_patterns: vec!["secret.#".to_string()],
                write_patterns: vec![],
            };
            let perms = DistinctGrant {
                public: perms_public,
                secret: perms_secret,
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(perms)
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::Injected => {
            // Grant for TopicProvider tests
            let perms = Permissions {
                read_patterns: vec!["injected.granted".to_string()],
                write_patterns: vec![],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::WritePublic => {
            // Grants for client writing tests
            let perms = Permissions {
                read_patterns: vec!["public.#".to_string()],
                write_patterns: vec!["writable_topic".to_string()],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::None => {
            // No permission granted
            let perms = Permissions {
                read_patterns: vec![],
                write_patterns: vec![],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::PublicAndOld => {
            // Grants over both a live record and an unregistered, persisted one
            let perms = Permissions {
                read_patterns: vec!["public.#".to_string(), "old.#".to_string()],
                write_patterns: vec![],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
        GrantType::OldRecord => {
            // Access to unregistered records but persist in store
            let perms = Permissions {
                read_patterns: vec!["old.#".to_string()],
                write_patterns: vec![],
            };
            AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(
                    WebSocketConnector::new()
                        .with_auth(FixedGrant(perms))
                        .bind(addr)
                        .path("/ws"),
                )
        }
    };

    keys_topics.iter().for_each(|(k, t)| {
        sb.configure::<Msg>(*k, |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .with_remote_access()
                .link_to(format!("ws://{}", t).as_str())
                .with_serializer(|_ctx, m: &Msg| Ok(serde_json::to_vec(m).expect("serialize")))
                .finish();
        });
    });

    // Extra keys for TopicProvider test
    for key in ["injected.granted", "injected.denied"] {
        sb.configure::<Inject>(key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 64 }) // don't coalesce successive values
                .with_remote_access()
                .link_to("ws://_") // placeholder; provider overrides per value
                .with_topic_provider(InjectTopic)
                .with_serializer(|_ctx, m: &Inject| {
                    Ok(serde_json::to_vec(&m.payload).expect("serialize"))
                })
                .finish();
        });
    }

    // Write need different config
    sb.configure::<Msg>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws://writable_topic")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Msg>(d).map_err(|e| e.to_string())
            })
            .finish();
    });

    // For query handler test
    if let Some(custom_handler) = query_handler {
        let handler_fn = custom_handler.get_handler_fn();
        sb.extensions_mut().insert(handler_fn);
    };

    let (server_db, server_runner) = sb.build().await.expect("build server db");
    tokio::spawn(server_runner.run());

    // Give the server a moment to bind before the client dials.
    wait_for_listen(addr).await;
    server_db
}

#[tokio::test]
async fn authentication_by_record_keys() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::Fixed, None).await;

    // Connect client
    let mut client = ws_connect(addr).await;

    //------------- Testing for correct record gating
    // Assert that record.list contains only "public.ledger"
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.list",
            "params": null,
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    let keys: Vec<&str> = reply["ok"]
        .as_array()
        .expect("record.list array")
        .iter()
        .map(|row| row["record_key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["public.ledger", "public.data"],
        "grant `public.#` must hide `sensor`"
    );
}

#[tokio::test]
async fn client_receives_only_records_in_perms() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Fixed, None).await;

    // Connect client
    let mut client = ws_connect(addr).await;
    // Client subscribes for topic "public_info"
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 2,
            "topic": "public_info",
        }),
    )
    .await;

    // Must wait till server registers client's subscription
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "2");

    // Server broadcast
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 1}));

    // Client receives
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["data"], json!({"v": 1}));

    // Server broadcast
    let _ = server_db.set_record_from_json("sensor", json!({"v": 99}));
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 2}));

    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["data"], json!({"v": 2}));
}

#[tokio::test]
async fn late_join_client_receives_only_records_in_perms() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Fixed, None).await;

    // A client as watcher, just for testing, no need in real use
    let mut watcher = ws_connect(addr).await;

    // The watcher subscribes to topics, just to make sure that server properly broadcast
    ws_send(
        &mut watcher,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "public_info",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut watcher, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Server broadcast
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 1}));

    // Make sure that message already broadcasted
    let ev = ws_recv_tag(&mut watcher, "event").await;
    assert_eq!(ev["data"], json!({"v": 1}));

    // Server broadcast
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 2}));
    let ev = ws_recv_tag(&mut watcher, "event").await;
    assert_eq!(ev["data"], json!({"v": 2}));

    let _ = server_db.set_record_from_json("public.data", json!({"v": 3}));
    let ev = ws_recv_tag(&mut watcher, "event").await;
    assert_eq!(ev["data"], json!({"v": 3}));

    // Late joining client
    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "public_info",
        }),
    )
    .await;

    // Must wait till server registers client's subscription
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Getting snapshot, 2 keys so we must loop till we get last snapshot
    let mut snapshot_data = Vec::new();
    loop {
        let snap = ws_recv_tag(&mut client, "snap").await;
        let last = snap
            .get("last")
            .unwrap_or(&Value::Bool(false))
            .as_bool()
            .unwrap();
        assert_eq!(snap["sub"], "1");
        assert_eq!(snap["topic"], "public_info");
        snapshot_data.push(snap["data"].clone());
        if last {
            break;
        }
    }

    // Assert that client get 2 snapshot for 2 record keys
    snapshot_data.sort_by_key(|d| d["v"].as_u64());
    assert_eq!(snapshot_data, [json!({"v": 2}), json!({"v": 3})]);
}

#[tokio::test]
async fn clients_disjoint_grants() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Distinct, None).await;

    // A client with no authorization will be denied
    assert!(try_ws_connect_with(addr, "&data_type=not_exist")
        .await
        .is_err());

    // Different clients could have different grants
    let mut client_public = ws_connect_with(addr, "&data_type=public").await;
    ws_send(
        &mut client_public,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "public_info",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client_public, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Client with grant to secret.#, even subscribing to public_info,
    // will receive nothing when server broadcasts to public.ledger
    let mut client_secret = ws_connect_with(addr, "&data_type=secret").await;
    ws_send(
        &mut client_secret,
        json!({
            "t": "sub",
            "id": 2,
            "topic": "public_info",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client_secret, "subscribed").await;
    assert_eq!(ack["sub"], "2");

    // Server broadcast to public.ledger
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 1}));

    // Public client receive public.ledger
    let ev = ws_recv_tag(&mut client_public, "event").await;
    assert_eq!(ev["data"], json!({"v": 1}));

    // Secret client receives nothing
    let ev = try_ws_recv_tag(&mut client_secret, "event").await;
    assert!(ev.is_err());
    if let Err(s) = ev {
        assert_eq!(s.as_str(), "recv timed out")
    };
}

#[tokio::test]
async fn client_wildcard_subscription_receives_public_only() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Fixed, None).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 1}));
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["data"], json!({"v": 1}));

    let _ = server_db.set_record_from_json("secret.sensor", json!({"v": 2}));
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 3}));
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["data"], json!({"v": 3}));

    // Late joining client also receive from public.ledger
    let mut client_late = ws_connect(addr).await;
    ws_send(
        &mut client_late,
        json!({
            "t": "sub",
            "id": 2,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client_late, "subscribed").await;
    assert_eq!(ack["sub"], "2");

    let snap = ws_recv_tag(&mut client_late, "snap").await;
    assert_eq!(snap["data"]["v"].as_u64(), Some(3));
}

#[tokio::test]
async fn topic_provider_injects_for_unsubscribed_client() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Injected, None).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Client has permissions to injected.granted, topic _ is injected by "a.b"
    let _ = server_db.set_record_from_json(
        "injected.granted",
        json!({
            "topic": "a.b",
            "payload": 1
        }),
    );
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["topic"], "a.b");
    assert_eq!(ev["data"], 1);

    // Client keep receiving from record, not topic
    let _ = server_db.set_record_from_json(
        "injected.granted",
        json!({
            "topic": "c.d",
            "payload": 2
        }),
    );
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["topic"], "c.d");
    assert_eq!(ev["data"], 2);

    // Client does not received from ungranted record
    let _ = server_db.set_record_from_json(
        "injected.denied",
        json!({
            "topic": "c.d",
            "payload": 3
        }),
    );
    let _ = server_db.set_record_from_json(
        "injected.granted",
        json!({
            "topic": "c.d",
            "payload": 4
        }),
    );
    let ev = ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev["topic"], "c.d");
    assert_eq!(ev["data"], 4);
}

#[tokio::test]
async fn record_query_uphold_record_grant() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::Fixed, Some(CustomQueryHandler::All)).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Query for all returned allowed records
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["ok"]["total"], 1);
    assert_eq!(
        reply["ok"]["records"][0]["topic"],
        "public.ledger".to_string()
    );

    // Query for not allow records returns none
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 2,
            "method": "record.query",
            "params": {"name": "secret.#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["ok"]["total"], 0);
}

#[tokio::test]
async fn record_query_denied_for_no_grant() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::None, None).await;

    let mut client = ws_connect(addr).await;

    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 2,
            "method": "record.query",
            "params": {"name": "secret.#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["err"], "denied");
}

#[tokio::test]
async fn subscribe_denied_for_no_grant() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::None, None).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;

    // The refusal is a `reply` carrying the subscribe id
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply, json!({"t": "reply", "id": 1, "err": "denied"}));
}

#[tokio::test]
async fn subscribe_accepted_for_grant_matching_no_registered_record() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::OldRecord, None).await;

    // Grant `old.#` matches no registered record: the client still subscribes
    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // ...but stays silent, as no granted record ever publishes
    let _ = server_db.set_record_from_json("public.ledger", json!({"v": 1}));
    let ev = try_ws_recv_tag(&mut client, "event").await;
    assert_eq!(ev, Err("recv timed out".to_string()));
}

#[tokio::test]
async fn record_query_returns_unregistered_record_in_store() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::OldRecord, Some(CustomQueryHandler::All)).await;

    let mut client = ws_connect(addr).await;

    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "old.#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["ok"]["total"], 1);
    assert_eq!(reply["ok"]["records"][0]["topic"], "old.sensor");
    assert_eq!(reply["ok"]["records"][0]["payload"], 3);

    // Still cannot read
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "public.#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["ok"]["total"], 0);
}

#[tokio::test]
async fn record_query_returns_partial_rows_across_grants() {
    let addr = free_addr();
    let _server_db =
        test_fixture(addr, GrantType::PublicAndOld, Some(CustomQueryHandler::All)).await;

    let mut client = ws_connect(addr).await;

    // `#` matches all three stored rows; the grants cover two of them,
    // one from a live record and one from an unregistered record
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["ok"]["total"], 2);
    let topics: Vec<&str> = reply["ok"]["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|r| r["topic"].as_str().unwrap())
        .collect();
    assert_eq!(topics, ["public.ledger", "old.sensor"]);
}

#[tokio::test]
async fn no_write_patterns_got_denied() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::Fixed, Some(CustomQueryHandler::All)).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    ws_send(
        &mut client,
        json!({
            "t": "write",
            "topic": "cfg",
            "payload": {"v": 1},
        }),
    )
    .await;

    // FIFO on the one connection: the pong proves the write frame was processed.
    ws_send(&mut client, json!({"t":"ping"})).await;
    assert_eq!(ws_recv(&mut client).await, json!({"t":"pong"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server_db.try_latest_as_json("cfg"), None);
}

#[tokio::test]
async fn write_patterns_uphold_topic() {
    let addr = free_addr();
    let server_db = test_fixture(addr, GrantType::WritePublic, Some(CustomQueryHandler::All)).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "writable_topic",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    ws_send(
        &mut client,
        json!({
            "t": "write",
            "topic": "writable_topic",
            "payload": {"v": 1},
        }),
    )
    .await;

    // FIFO on the one connection: the pong proves the write frame was processed.
    ws_send(&mut client, json!({"t":"ping"})).await;
    assert_eq!(ws_recv(&mut client).await, json!({"t":"pong"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server_db.try_latest_as_json("cfg"), Some(json!({"v": 1})));
}

#[tokio::test]
async fn query_handler_returns_empty_handled_as_empty() {
    let addr = free_addr();
    let _server_db = test_fixture(addr, GrantType::Fixed, Some(CustomQueryHandler::NoRecord)).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Query for all returned allowed records
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["ok"]["total"], 0);
}

#[tokio::test]
async fn query_handler_malformed_handled_as_err() {
    let addr = free_addr();
    let _server_db =
        test_fixture(addr, GrantType::Fixed, Some(CustomQueryHandler::Malformed)).await;

    let mut client = ws_connect(addr).await;
    ws_send(
        &mut client,
        json!({
            "t": "sub",
            "id": 1,
            "topic": "#",
        }),
    )
    .await;
    let ack = ws_recv_tag(&mut client, "subscribed").await;
    assert_eq!(ack["sub"], "1");

    // Query for all returned allowed records
    ws_send(
        &mut client,
        json!({
            "t": "req",
            "id": 1,
            "method": "record.query",
            "params": {"name": "#"},
        }),
    )
    .await;
    let reply = ws_recv_tag(&mut client, "reply").await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["err"], "internal".to_string());
}
