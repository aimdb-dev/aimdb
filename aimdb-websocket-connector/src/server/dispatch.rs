//! WS server [`Dispatch`] + [`Session`].
//!
//! [`WsDispatch`] is the shared half (one `Arc<dyn Dispatch>` per server):
//! `authenticate` reads the identity pre-resolved at the HTTP upgrade (in
//! [`PeerInfo`]`::ext`), `open` mints a per-connection [`WsSession`] homing the
//! application surface (the [`ClientManager`] bus handle, auth principal, query
//! handler).
//!
//! The wire is AimX ([`aimdb_core::session::aimx::AimxCodec`]); this dispatch
//! only supplies WS-specific semantics on top of the shared vocabulary:
//! HTTP-upgrade auth, the bus-backed subscribe, and the `record.query` /
//! `record.list` calls. The `Subscribed` ack and late-join `Snapshot`s are
//! engine emissions; this session only supplies the snapshot bytes and the
//! subscription stream.

use std::collections::HashMap;
use std::sync::Arc;

use aimdb_core::remote::{QueryHandlerFn, QueryHandlerParams};
use aimdb_core::session::Session;
use aimdb_core::{
    AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, SessionCtx, SubUpdate,
};
use serde_json::Value;

use super::{
    auth::{AuthHandler, ClientId, ClientInfo, Permissions},
    client_manager::{ClientManager, ConnectionGuard},
    session::{QueryHandler, Router, SnapshotProvider},
};

/// The shared WS dispatch — one `Arc<dyn Dispatch>` per server.
pub struct WsDispatch {
    /// Cheap-clone db handle — resolves the Extensions-registered
    /// `QueryHandlerFn` when no custom query handler is plugged in.
    pub(crate) db: aimdb_core::builder::AimDb,
    pub(crate) client_mgr: ClientManager,
    pub(crate) snapshot_provider: Arc<dyn SnapshotProvider>,
    pub(crate) query_handler: Option<Arc<dyn QueryHandler>>,
    pub(crate) router: Arc<Router>,
    /// Record `type_id` string → data-contract schema name, used to stamp
    /// `schema_type` onto the `record.list` rows core hands back.
    pub(crate) schema_by_type: Arc<HashMap<String, String>>,
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
            db: self.db.clone(),
            client_mgr: self.client_mgr.clone(),
            snapshot_provider: self.snapshot_provider.clone(),
            query_handler: self.query_handler.clone(),
            router: self.router.clone(),
            schema_by_type: self.schema_by_type.clone(),
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
    db: aimdb_core::builder::AimDb,
    client_mgr: ClientManager,
    snapshot_provider: Arc<dyn SnapshotProvider>,
    query_handler: Option<Arc<dyn QueryHandler>>,
    router: Arc<Router>,
    schema_by_type: Arc<HashMap<String, String>>,
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
            let value = match method {
                "record.list" => {
                    // Same `RecordMetadata` rows core serves over every other
                    // transport; the connector only fills in the schema name core
                    // can't resolve.
                    let mut records = self.db.list_records();
                    for record in &mut records {
                        if record.schema_type.is_none() {
                            if let Some(name) = self.schema_by_type.get(&record.type_id) {
                                record.schema_type = Some(name.clone());
                            }
                        }
                    }
                    serde_json::json!(records)
                }
                "record.query" => {
                    let params: Value = serde_json::from_slice(&params).unwrap_or(Value::Null);
                    self.record_query(params).await?
                }
                _ => return Err(RpcError::NotFound),
            };
            let bytes = serde_json::to_vec(&value).map_err(|_| RpcError::Internal)?;
            Ok(Payload::from(bytes.as_slice()))
        })
    }

    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, SubUpdate>, RpcError>> {
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

    fn snapshots(&mut self, topic: &str) -> Vec<(String, Payload)> {
        if !self.late_join {
            return Vec::new();
        }
        self.snapshot_provider
            .snapshots(topic)
            .into_iter()
            .map(|(topic, bytes)| (topic, Payload::from(bytes.as_slice())))
            .collect()
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

impl WsSession {
    /// `record.query` with the shared `{name, limit, start, end}` params and
    /// `{records, total}` result: a plugged-in
    /// [`QueryHandler`] wins; otherwise delegate to the Extensions-registered
    /// `QueryHandlerFn` (`with_persistence`); neither → `NotFound`.
    async fn record_query(&self, params: Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .to_string();
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok());
        let start = params.get("start").and_then(|v| v.as_u64());
        let end = params.get("end").and_then(|v| v.as_u64());

        if let Some(handler) = &self.query_handler {
            let (records, total) = handler
                .handle_query(&name, start, end, limit)
                .await
                .map_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("record.query handler failed: {}", _e);
                    RpcError::Internal
                })?;
            return Ok(serde_json::json!({ "records": records, "total": total }));
        }

        let handler_fut = {
            let handler = self
                .db
                .extensions()
                .get::<QueryHandlerFn>()
                .ok_or(RpcError::NotFound)?;
            handler(QueryHandlerParams {
                name,
                limit,
                start,
                end,
            })
        };
        handler_fut.await.map_err(|_e| {
            #[cfg(feature = "tracing")]
            tracing::warn!("record.query persistence handler failed: {}", _e);
            RpcError::Internal
        })
    }
}
