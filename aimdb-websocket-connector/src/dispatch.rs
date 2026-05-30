//! WS server [`Dispatch`] + [`Session`] (Phase 4 — doc 039 § 3).
//!
//! [`WsDispatch`] is the shared half (one `Arc<dyn Dispatch>` per server):
//! `authenticate` reads the identity pre-resolved at the HTTP upgrade (carried in
//! [`PeerInfo`]`::ext`), `open` mints a per-connection [`WsSession`]. The session
//! threads `&mut self` from `run_session` and homes the application surface — the
//! [`ClientManager`] bus handle, the auth principal, the query handler — exactly
//! as `AimxSession` homes `drain_readers`.
//!
//! The id↔topic bookkeeping does **not** live here — it lives in the
//! per-connection [`WsCodec`](crate::codec) (doc 039 § 2). The explicit
//! `Subscribed` ack and late-join `Snapshot` are engine emissions
//! (`acks_subscribe` + `Session::snapshot`); this session only supplies the
//! snapshot bytes and the filtered subscription stream.

use std::any::Any;
use std::sync::Arc;

use aimdb_core::session::Session;
use aimdb_core::{AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, SessionCtx};
use aimdb_ws_protocol::TopicInfo;

use crate::{
    auth::{AuthHandler, ClientId, ClientInfo, Permissions},
    client_manager::{ClientManager, ConnectionGuard},
    protocol::{ClientMessage, ErrorCode, ServerMessage},
    session::{QueryHandler, Router, SnapshotProvider},
};

/// The shared WS dispatch — one `Arc<dyn Dispatch>` per server.
pub struct WsDispatch {
    pub(crate) client_mgr: ClientManager,
    pub(crate) snapshot_provider: Arc<dyn SnapshotProvider>,
    pub(crate) query_handler: Arc<dyn QueryHandler>,
    pub(crate) router: Arc<Router>,
    pub(crate) known_topics: Arc<Vec<TopicInfo>>,
    pub(crate) auth: Arc<dyn AuthHandler>,
    pub(crate) late_join: bool,
    pub(crate) runtime_ctx: Option<Arc<dyn Any + Send + Sync>>,
}

impl Dispatch for WsDispatch {
    fn authenticate<'a>(
        &'a self,
        peer: &'a PeerInfo,
        _first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        // Identity is pre-resolved at the HTTP upgrade and carried in `PeerInfo`
        // as a `ClientInfo` (WS-style `reads_hello:false`, doc 039 § 4).
        let info = peer.ext_as::<ClientInfo>();
        Box::pin(async move {
            match info {
                Some(info) => Ok(SessionCtx::with_ext(info)),
                None => Err(AuthError::Unauthorized),
            }
        })
    }

    fn open(&self, ctx: &SessionCtx) -> Box<dyn Session> {
        let info = ctx.ext_as::<ClientInfo>().unwrap_or_else(|| {
            // Should not happen (authenticate populates it); deny-all fallback.
            Arc::new(ClientInfo {
                id: ClientId(0),
                remote_addr: ([0, 0, 0, 0], 0).into(),
                permissions: Permissions::default(),
            })
        });
        Box::new(WsSession {
            client_mgr: self.client_mgr.clone(),
            snapshot_provider: self.snapshot_provider.clone(),
            query_handler: self.query_handler.clone(),
            router: self.router.clone(),
            known_topics: self.known_topics.clone(),
            auth: self.auth.clone(),
            late_join: self.late_join,
            runtime_ctx: self.runtime_ctx.clone(),
            info,
            _conn_guard: self.client_mgr.connection_guard(),
        })
    }
}

/// One connection's per-session state (owned by the engine, `&mut`-threaded).
struct WsSession {
    client_mgr: ClientManager,
    snapshot_provider: Arc<dyn SnapshotProvider>,
    query_handler: Arc<dyn QueryHandler>,
    router: Arc<Router>,
    known_topics: Arc<Vec<TopicInfo>>,
    auth: Arc<dyn AuthHandler>,
    late_join: bool,
    runtime_ctx: Option<Arc<dyn Any + Send + Sync>>,
    info: Arc<ClientInfo>,
    /// Decrements the live-connection count on drop.
    _conn_guard: ConnectionGuard,
}

impl Session for WsSession {
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        Box::pin(async move {
            let msg: ClientMessage =
                serde_json::from_slice(&params).map_err(|_| RpcError::Internal)?;
            let response = match (method, msg) {
                (
                    "query",
                    ClientMessage::Query {
                        id,
                        pattern,
                        from,
                        to,
                        limit,
                    },
                ) => match self
                    .query_handler
                    .handle_query(&pattern, from, to, limit)
                    .await
                {
                    Ok((records, total)) => ServerMessage::QueryResult {
                        id,
                        records,
                        total,
                    },
                    Err(message) => ServerMessage::Error {
                        code: ErrorCode::ServerError,
                        topic: None,
                        message,
                    },
                },
                ("list_topics", ClientMessage::ListTopics { id }) => ServerMessage::TopicList {
                    id,
                    topics: (*self.known_topics).clone(),
                },
                _ => return Err(RpcError::NotFound),
            };
            // The codec writes this complete `ServerMessage` verbatim (doc 039 § 2).
            let bytes = serde_json::to_vec(&response).map_err(|_| RpcError::Internal)?;
            Ok(Payload::from(bytes.as_slice()))
        })
    }

    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
        Box::pin(async move {
            // Per-operation authorization — the full async `AuthHandler` hook, so
            // a custom `authorize_subscribe` (per-topic ACL, token introspection)
            // is honored, not just the static permission set.
            if !self.auth.authorize_subscribe(&self.info, topic).await {
                return Err(RpcError::Denied);
            }
            // Register on the shared bus; the engine owns the returned stream and
            // drops it on Unsubscribe/teardown (the bus prunes the dead entry).
            let (_sub_id, stream) = self.client_mgr.subscribe(topic);
            Ok(stream)
        })
    }

    fn snapshot(&mut self, topic: &str) -> Option<Payload> {
        if !self.late_join {
            return None;
        }
        self.snapshot_provider
            .snapshot(topic)
            .map(|bytes| Payload::from(bytes.as_slice()))
    }

    fn write<'a>(
        &'a mut self,
        topic: &'a str,
        payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>> {
        Box::pin(async move {
            if !self.auth.authorize_write(&self.info, topic).await {
                return Err(RpcError::Denied);
            }
            self.router
                .route(topic, &payload, self.runtime_ctx.as_ref())
                .await
                .map_err(|_| RpcError::Internal)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use aimdb_core::session::{run_session, SessionConfig};
    use aimdb_core::router::RouterBuilder;
    use aimdb_core::{Connection, SessionLimits, TransportResult};
    use tokio::sync::mpsc;

    use crate::auth::{NoAuth, Permissions};
    use crate::codec::WsCodec;
    use crate::session::NoQuery;

    /// A mock [`Connection`]: inbound frames arrive on a channel, outbound frames
    /// are captured. Closing the channel ends the session.
    struct MockConn {
        rx: mpsc::UnboundedReceiver<Vec<u8>>,
        out: Arc<Mutex<Vec<Vec<u8>>>>,
        peer: PeerInfo,
    }

    impl Connection for MockConn {
        fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
            Box::pin(async move { Ok(self.rx.recv().await) })
        }
        fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
            let out = self.out.clone();
            let frame = frame.to_vec();
            Box::pin(async move {
                out.lock().unwrap().push(frame);
                Ok(())
            })
        }
        fn peer(&self) -> &PeerInfo {
            &self.peer
        }
    }

    struct OneSnap(String, Vec<u8>);
    impl SnapshotProvider for OneSnap {
        fn snapshot(&self, topic: &str) -> Option<Vec<u8>> {
            (topic == self.0).then(|| self.1.clone())
        }
    }

    fn dispatch_with(snapshot: Arc<dyn SnapshotProvider>) -> Arc<WsDispatch> {
        Arc::new(WsDispatch {
            client_mgr: ClientManager::new(false),
            snapshot_provider: snapshot,
            query_handler: Arc::new(NoQuery),
            router: Arc::new(RouterBuilder::from_routes(Vec::new()).build()),
            known_topics: Arc::new(Vec::new()),
            auth: Arc::new(NoAuth),
            late_join: true,
            runtime_ctx: None,
        })
    }

    fn allow_all_peer() -> PeerInfo {
        PeerInfo::default().with_ext(Arc::new(ClientInfo {
            id: ClientId(1),
            remote_addr: ([127, 0, 0, 1], 0).into(),
            permissions: Permissions::allow_all(),
        }))
    }

    fn parse(out: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<ServerMessage> {
        out.lock()
            .unwrap()
            .iter()
            .map(|f| serde_json::from_slice(f).unwrap())
            .collect()
    }

    // run_session drives the real codec + dispatch: subscribe → ack + late-join
    // snapshot, then a bus broadcast fans out as a Data frame.
    #[tokio::test]
    async fn subscribe_ack_snapshot_and_fanout() {
        let dispatch =
            dispatch_with(Arc::new(OneSnap("sensors/temp".into(), b"\"last\"".to_vec())));
        let mgr = dispatch.client_mgr.clone();

        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let out = Arc::new(Mutex::new(Vec::new()));
        let conn = MockConn {
            rx,
            out: out.clone(),
            peer: allow_all_peer(),
        };

        let task = {
            let dispatch = dispatch.clone();
            tokio::spawn(async move {
                let codec = WsCodec::new();
                let config = SessionConfig {
                    limits: SessionLimits::default(),
                    reads_hello: false,
                    acks_subscribe: true,
                };
                run_session(Box::new(conn), &codec, dispatch.as_ref(), &config).await;
            })
        };

        // Subscribe to an exact topic so the snapshot provider matches.
        tx.send(
            serde_json::to_vec(&ClientMessage::Subscribe {
                topics: vec!["sensors/temp".into()],
            })
            .unwrap(),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Ack + snapshot should have been emitted, in order.
        let msgs = parse(&out);
        assert!(matches!(&msgs[0], ServerMessage::Subscribed { topics } if topics == &vec!["sensors/temp".to_string()]));
        assert!(matches!(&msgs[1], ServerMessage::Snapshot { topic, .. } if topic == "sensors/temp"));

        // A bus broadcast now fans out to this subscription as a Data frame.
        mgr.broadcast("sensors/temp", b"\"22.5\"").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let msgs = parse(&out);
        let data = msgs
            .iter()
            .find_map(|m| match m {
                ServerMessage::Data { topic, payload, .. } => Some((topic.clone(), payload.clone())),
                _ => None,
            })
            .expect("a Data frame");
        assert_eq!(data.0, "sensors/temp");
        assert_eq!(data.1, Some(serde_json::json!("22.5")));

        drop(tx); // close the connection → end the session
        let _ = task.await;
    }

    // One broadcast reaches N subscribed connections (the fan-out bridge).
    #[tokio::test]
    async fn fanout_to_multiple_connections() {
        let dispatch = dispatch_with(Arc::new(crate::session::NoSnapshot));
        let mgr = dispatch.client_mgr.clone();

        let mut conns = Vec::new();
        for _ in 0..3 {
            let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let out = Arc::new(Mutex::new(Vec::new()));
            let conn = MockConn {
                rx,
                out: out.clone(),
                peer: allow_all_peer(),
            };
            let dispatch = dispatch.clone();
            let task = tokio::spawn(async move {
                let codec = WsCodec::new();
                let config = SessionConfig {
                    limits: SessionLimits::default(),
                    reads_hello: false,
                    acks_subscribe: true,
                };
                run_session(Box::new(conn), &codec, dispatch.as_ref(), &config).await;
            });
            tx.send(
                serde_json::to_vec(&ClientMessage::Subscribe {
                    topics: vec!["weather/#".into()],
                })
                .unwrap(),
            )
            .unwrap();
            conns.push((tx, out, task));
        }
        // Wait until all three subscriptions have registered on the bus.
        for _ in 0..50 {
            if mgr.subscription_count() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(mgr.subscription_count(), 3);

        mgr.broadcast("weather/vienna", b"\"sunny\"").await;

        for (tx, out, task) in conns {
            let mut got_data = false;
            for _ in 0..50 {
                got_data = parse(&out).iter().any(
                    |m| matches!(m, ServerMessage::Data { topic, .. } if topic == "weather/vienna"),
                );
                if got_data {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(got_data, "each connection should receive the broadcast");
            drop(tx);
            let _ = task.await;
        }
    }
}
