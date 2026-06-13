//! WS server [`Dispatch`] + [`Session`].
//!
//! [`WsDispatch`] is the shared half (one `Arc<dyn Dispatch>` per server):
//! `authenticate` reads the identity pre-resolved at the HTTP upgrade (in
//! [`PeerInfo`]`::ext`), `open` mints a per-connection [`WsSession`] homing the
//! application surface (the [`ClientManager`] bus handle, auth principal, query
//! handler).
//!
//! The id↔topic bookkeeping lives in the per-connection [`WsCodec`](crate::codec),
//! not here. The `Subscribed` ack and late-join `Snapshot` are engine emissions;
//! this session only supplies the snapshot bytes and the subscription stream.

use std::sync::Arc;

use aimdb_core::session::Session;
use aimdb_core::{AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, SessionCtx};
use aimdb_ws_protocol::TopicInfo;

use crate::protocol::{ClientMessage, ErrorCode, ServerMessage};

use super::{
    auth::{AuthHandler, ClientId, ClientInfo, Permissions},
    client_manager::{ClientManager, ConnectionGuard},
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
    pub(crate) runtime_ctx: aimdb_core::RuntimeContext,
}

impl Dispatch for WsDispatch {
    fn authenticate<'a>(
        &'a self,
        peer: &'a PeerInfo,
        _first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        // Identity is pre-resolved at the HTTP upgrade and carried in `PeerInfo`.
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
    runtime_ctx: aimdb_core::RuntimeContext,
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
                    Ok((records, total)) => ServerMessage::QueryResult { id, records, total },
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
            // The codec writes this complete `ServerMessage` verbatim.
            let bytes = serde_json::to_vec(&response).map_err(|_| RpcError::Internal)?;
            Ok(Payload::from(bytes.as_slice()))
        })
    }

    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
        Box::pin(async move {
            // Per-operation authorization via the async `AuthHandler` hook.
            if !self.auth.authorize_subscribe(&self.info, topic).await {
                return Err(RpcError::Denied);
            }
            // Register on the shared bus; the engine owns and drops the stream.
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
                .route(topic, &payload, &self.runtime_ctx)
                .map_err(|_| RpcError::Internal)
        })
    }
}
