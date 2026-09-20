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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aimdb_core::remote::{QueryHandlerFn, QueryHandlerParams, QueryRecord, QUERY_ALL_PATTERN};
use aimdb_core::session::Session;
use aimdb_core::{
    AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, SessionCtx, SubUpdate,
};
use serde_json::Value;

use super::{
    auth::{AuthHandler, ClientId, ClientInfo, Permissions, RecordsBits},
    client_manager::ClientManager,
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

    /// Record key in registration order
    pub(crate) records: Arc<Vec<String>>,
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
                record_perms: Arc::new(RecordsBits::new(0)),
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
            records: self.records.clone(),
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
    records: Arc<Vec<String>>,
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
                    // transport — the whole database, so narrow it to the rows
                    // this client may see; the connector only fills in the schema
                    // name core can't resolve.
                    let mut records = Vec::new();
                    for mut record in self.db.list_records() {
                        if !self.info.record_perms.is_allowed(record.record_id as usize) {
                            continue;
                        }
                        if record.schema_type.is_none() {
                            if let Some(name) = self.schema_by_type.get(&record.type_id) {
                                record.schema_type = Some(name.clone());
                            }
                        }
                        records.push(record);
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

    /// Called when a client subscribes to a topic
    /// As `Auth` no longer gate topic (permissions lie in record keys),
    /// a client could subscribe to a topic which has no associated records.
    /// As a result, no message could reach that client.
    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, SubUpdate>, RpcError>> {
        // A client authorized for zero records must be denied
        if !self.info.record_perms.has_permissions() {
            return Box::pin(async move { Err(RpcError::Denied) });
        };

        Box::pin(async move {
            // Register on the shared bus; the engine owns and drops the stream.
            // A topic always come with a record, so the client always receive a stream,
            // message broadcasting will check for granted permissions (allowed records) later
            let (_sub_id, stream) = self
                .client_mgr
                .subscribe(topic, self.info.record_perms.clone());
            Ok(stream)
        })
    }

    fn snapshots(&mut self, topic: &str) -> Vec<(String, Payload)> {
        if !self.late_join {
            return Vec::new();
        }

        // Filtered by client's record permissions
        self.snapshot_provider
            .snapshots(topic)
            .into_iter()
            .filter(|(record_id, _, _)| self.info.record_perms.is_allowed(*record_id))
            .map(|(_, topic, bytes)| (topic, Payload::from(bytes.as_slice())))
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
    /// Final query outputs are results of:
    /// 1. Query handler got passed in pattern name;
    /// 2. Outputs from the query handler got filtered again by allow records
    ///    which incorporate per-client permissions.
    ///
    /// Behaviors:
    /// - A client having no permission, or granted permissions and pattern name not overlapping,
    ///   will have its query denied;
    /// - A client having permissions and pattern name overlapping, may have partial query output,
    ///   meaning the pattern name partially covers the query outputs.
    async fn record_query(&self, params: Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(QUERY_ALL_PATTERN)
            .to_string();
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok());
        let start = params.get("start").and_then(|v| v.as_u64());
        let end = params.get("end").and_then(|v| v.as_u64());

        // Allowed record keys, filtered by both query name and record bitmasks
        let allowed_names: HashSet<String> = self
            .records
            .iter()
            .enumerate()
            .filter(|(i, _r)| self.info.record_perms.is_allowed(*i))
            .filter(|(_i, r)| aimdb_core::topic_matches(&name, r.as_str()))
            .map(|(_i, r)| r.clone())
            .collect();

        if allowed_names.is_empty() {
            return Err(RpcError::Denied);
        };

        // The query is handled here
        let records: Vec<QueryRecord> = if let Some(handler) = &self.query_handler {
            let (records, _total) = handler
                .handle_query(&name, start, end, limit)
                .await
                .map_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("record.query handler failed: {}", _e);
                    RpcError::Internal
                })?;
            records
        } else {
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
            let mut values = handler_fut.await.map_err(|_e| {
                #[cfg(feature = "tracing")]
                tracing::warn!("record.query persistence handler failed: {}", _e);
                RpcError::Internal
            })?;

            serde_json::from_value::<Vec<QueryRecord>>(
                values
                    .get_mut("records")
                    .map(Value::take)
                    .unwrap_or(Value::Null),
            )
            .map_err(|_e| {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "record.query failed converting from json to QueryRecord: {}",
                    _e
                );
                RpcError::Internal
            })?
        };

        // Query results need to be filtered by allowed record keys
        // the results could be:
        // - empty: if client is allowed to query, but nothing to query
        // - partial: if the name partially matches the permission bitmasks, e.g. allowed names
        // - full: if the name totally matches the allowed names
        let records: Vec<QueryRecord> = records
            .into_iter()
            .filter(|r| allowed_names.contains(&r.topic))
            .collect();

        Ok(serde_json::json!({ "records": records, "total": records.len() }))
    }
}
