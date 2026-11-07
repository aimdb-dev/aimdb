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

use crate::events::{report, WebSocketEvent};

use std::collections::HashMap;
use std::sync::Arc;

use aimdb_core::remote::{PostLinkError, QueryHandlerFn, QueryHandlerParams};
use aimdb_core::session::Session;
use aimdb_core::{
    AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, SessionCtx, SubUpdate,
};
use serde_json::Value;

use super::{
    auth::{AuthHandler, ClientId, ClientInfo, Permissions, RecordsBits},
    client_manager::ClientManager,
    session::SnapshotProvider,
};

/// The shared WS dispatch — one `Arc<dyn Dispatch>` per server.
pub struct WsDispatch {
    /// Cheap-clone db handle — resolves the Extensions-registered
    /// `QueryHandlerFn` for `record.query`.
    pub(crate) db: aimdb_core::builder::AimDb,
    pub(crate) client_mgr: ClientManager,
    pub(crate) snapshot_provider: Arc<dyn SnapshotProvider>,
    pub(crate) inbound: aimdb_core::InboundDispatch,
    /// Record `type_id` string → data-contract schema name, used to stamp
    /// `schema_type` onto the `record.list` rows core hands back.
    pub(crate) schema_by_type: Arc<HashMap<String, String>>,
    pub(crate) auth: Arc<dyn AuthHandler>,
    pub(crate) late_join: bool,
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
            inbound: self.inbound.clone(),
            schema_by_type: self.schema_by_type.clone(),
            auth: self.auth.clone(),
            late_join: self.late_join,
            info,
        })
    }
}

/// One connection's per-session state (owned by the engine, `&mut`-threaded).
struct WsSession {
    db: aimdb_core::builder::AimDb,
    client_mgr: ClientManager,
    snapshot_provider: Arc<dyn SnapshotProvider>,
    inbound: aimdb_core::InboundDispatch,
    schema_by_type: Arc<HashMap<String, String>>,
    auth: Arc<dyn AuthHandler>,
    late_join: bool,
    info: Arc<ClientInfo>,
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
                // Identity (3.1): the same replies as every other AimX
                // transport, built by core. Read access, like `subscribe`.
                "instance.info" | "instance.post_link"
                    if self.info.permissions.read_patterns.is_empty() =>
                {
                    return Err(RpcError::Denied);
                }
                "instance.info" => {
                    let reply = aimdb_core::remote::instance_info_reply(&self.db);
                    return Ok(Payload::from(reply.as_slice()));
                }
                "instance.post_link" => {
                    let params: Value = serde_json::from_slice(&params).unwrap_or(Value::Null);
                    return aimdb_core::remote::post_link_chunk_reply(&self.db, &params)
                        .map(|reply| Payload::from(reply.as_slice()))
                        .map_err(|e| match e {
                            PostLinkError::NotWritten => RpcError::NotFound,
                            PostLinkError::InvalidParams => RpcError::Internal,
                        });
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
        // Deny subscription for no-grant clients,
        // as clients having grants could have empty permission bitmap.
        if self.info.permissions.read_patterns.is_empty() {
            return Box::pin(async move { Err(RpcError::Denied) });
        }

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

    fn snapshots(&mut self, topic: &str) -> Vec<(String, Payload, aimdb_core::Stamp)> {
        if !self.late_join {
            return Vec::new();
        }

        // Filtered by client's record permissions
        self.snapshot_provider
            .snapshots(topic)
            .into_iter()
            .filter(|(record_id, _, _, _)| self.info.record_perms.is_allowed(*record_id))
            .map(|(_, topic, bytes, ts)| (topic, Payload::from(bytes.as_slice()), ts))
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
            self.inbound.dispatch(topic, &payload);
            Ok(())
        })
    }
}

impl WsSession {
    /// `record.query` with the shared `{name, limit, start, end}` params and
    /// `{records}` result, answered by the Extensions-registered
    /// `QueryHandlerFn` (`with_persistence`), as on every transport; without
    /// one → `NotFound`.
    /// A client having no grant will have its query denied.
    /// Grants allow clients to query both live and persist records.
    async fn record_query(&self, params: Value) -> Result<Value, RpcError> {
        // No-grant clients have their requests denied
        // Grant clients whose perms matching persisted records,
        // but matching no live records, are not denied
        if self.info.permissions.read_patterns.is_empty() {
            return Err(RpcError::Denied);
        };

        // The query is handled here
        let handler_fut = {
            let handler = self
                .db
                .extensions()
                .get::<QueryHandlerFn>()
                .ok_or(RpcError::NotFound)?;
            handler(QueryHandlerParams::from_json(&params))
        };
        let records = handler_fut.await.map_err(|_e| {
            report(WebSocketEvent::PersistenceQueryFailed { error: &_e });
            RpcError::Internal
        })?;

        // Drop the rows the client's grants don't cover.
        let records = self.auth.authorize_query_record(&self.info, records);

        Ok(serde_json::json!({ "records": records }))
    }
}
