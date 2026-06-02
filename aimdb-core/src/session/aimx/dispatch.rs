//! AimX server dispatch (`std`-only) — the method semantics of AimX remote
//! access, served on the shared session engine (`serve`/`run_session`).
//!
//! `std`-gated because it reaches into core's `record.list` / JSON API (the
//! `AnyRecord` JSON + metadata methods). A transport pairs this dispatch with the
//! generic [`SessionServerConnector`](crate::session::SessionServerConnector) —
//! see `aimdb-uds-connector`'s `UdsServer`.
//!
//! The role is split in two:
//! - [`AimxDispatch`] — the shared half (one `Arc` per server): peer-only
//!   `authenticate` + an `open` factory.
//! - `AimxSession` — the per-connection half the engine owns by value, homing
//!   `record.drain`'s lazy per-record cursors (`drain_readers`).
//!
//! Param shapes follow the client ([`aimdb_client::AimxConnection`]):
//! `record.get`/`record.set` take `{name[, value]}`, `write` takes `{value}`.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::buffer::JsonBufferReader;
use crate::remote::{AimxConfig, RecordMetadata, SecurityPolicy, WelcomeMessage};
use crate::session::{
    AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo, RpcError, Session, SessionCtx,
};
use crate::{AimDb, DbError, RuntimeAdapter};

/// The shared AimX dispatch — `authenticate` (peer-only) + the `AimxSession`
/// factory. One `Arc<AimxDispatch>` is shared across every accepted connection.
pub struct AimxDispatch<R: RuntimeAdapter> {
    db: Arc<AimDb<R>>,
    config: Arc<AimxConfig>,
}

impl<R: RuntimeAdapter> AimxDispatch<R> {
    /// Build a dispatch over `db` with the given remote-access `config`.
    pub fn new(db: Arc<AimDb<R>>, config: AimxConfig) -> Self {
        Self {
            db,
            config: Arc::new(config),
        }
    }
}

impl<R> Dispatch for AimxDispatch<R>
where
    R: RuntimeAdapter + 'static,
{
    fn authenticate<'a>(
        &'a self,
        _peer: &'a PeerInfo,
        _first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        // Peer-only: AimX over UDS relies on socket file permissions for access
        // control. Permission policy (ReadOnly / writable_records) is enforced
        // per-call from the config.
        Box::pin(async { Ok(SessionCtx::default()) })
    }

    fn open(&self, _ctx: &SessionCtx) -> Box<dyn Session> {
        Box::new(AimxSession {
            db: self.db.clone(),
            config: self.config.clone(),
            drain_readers: HashMap::new(),
        })
    }
}

/// One AimX connection's mutable dispatch state. The engine owns this by value
/// and threads `&mut self` into each method, so `drain_readers` (lazy per-record
/// cursors) need no lock.
struct AimxSession<R: RuntimeAdapter> {
    db: Arc<AimDb<R>>,
    config: Arc<AimxConfig>,
    /// Per-record drain readers, created lazily on first `record.drain`.
    drain_readers: HashMap<String, Box<dyn JsonBufferReader + Send>>,
}

impl<R> Session for AimxSession<R>
where
    R: RuntimeAdapter + 'static,
{
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        Box::pin(async move {
            let params: Value = serde_json::from_slice(&params).unwrap_or(Value::Null);
            self.dispatch_call(method, params)
                .await
                .map(|v| to_payload(&v))
        })
    }

    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
        // The engine owns the subscription lifecycle and the per-connection cap;
        // AimX has no async authorization, so this is a trivial wrapper.
        Box::pin(async move {
            let stream = crate::remote::stream::stream_record_updates(&self.db, topic)
                .map_err(map_db_err)?;
            Ok(Box::pin(stream.map(|v| to_payload(&v))) as BoxStream<'static, Payload>)
        })
    }

    fn write<'a>(
        &'a mut self,
        topic: &'a str,
        payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>> {
        Box::pin(async move {
            self.ensure_writable(topic)?;
            // The v2 client wraps the value as `{"value": <v>}`; fall back to the
            // whole payload if the wrapper is absent.
            let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
            let value = v.get("value").cloned().unwrap_or(v);
            // Routes through the producer/arbiter path — single-writer-per-key
            // stays enforced inside `set_record_from_json`.
            self.db
                .set_record_from_json(topic, value)
                .map_err(map_db_err)
        })
    }
}

impl<R> AimxSession<R>
where
    R: RuntimeAdapter + 'static,
{
    /// Match the method and produce its JSON result (or an [`RpcError`]).
    async fn dispatch_call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "hello" => Ok(self.welcome()),
            "record.list" => Ok(json!(self.db.list_records())),
            "record.get" => {
                let name = str_field(&params, "name").ok_or(RpcError::NotFound)?;
                self.record_get(&name)
            }
            "record.set" => self.record_set(params),
            "record.drain" => self.record_drain(params),
            "record.query" => self
                .record_query(params)?
                .await
                .map_err(|_| RpcError::Internal),
            "graph.nodes" => Ok(json!(self.db.inner().dependency_graph().nodes)),
            "graph.edges" => Ok(json!(self.db.inner().dependency_graph().edges)),
            "graph.topo_order" => Ok(json!(self.db.inner().dependency_graph().topo_order())),
            #[cfg(feature = "profiling")]
            "profiling.reset" => {
                self.ensure_write_permission()?;
                self.db.reset_stage_profiling();
                Ok(json!({ "reset": true }))
            }
            #[cfg(feature = "metrics")]
            "buffer_metrics.reset" => {
                self.ensure_write_permission()?;
                self.db.reset_buffer_metrics();
                Ok(json!({ "reset": true }))
            }
            _ => Err(RpcError::NotFound),
        }
    }

    /// `record.set` (RPC): permission-checked write that echoes the new value.
    fn record_set(&self, params: Value) -> Result<Value, RpcError> {
        let name = str_field(&params, "name").ok_or(RpcError::Internal)?;
        let value = params.get("value").cloned().ok_or(RpcError::Internal)?;
        self.ensure_writable(&name)?;
        self.db
            .set_record_from_json(&name, value)
            .map_err(map_db_err)?;
        // Echo the updated value when available (matches the legacy reply shape).
        Ok(match self.db.try_latest_as_json(&name) {
            Some(updated) => json!({ "status": "success", "value": updated }),
            None => json!({ "status": "success" }),
        })
    }

    /// `record.get`: the record's current value.
    ///
    /// A `SingleLatest`/state record exposes a non-destructive canonical latest
    /// ([`try_latest_as_json`](crate::AimDb::try_latest_as_json)). A ring
    /// ([`SpmcRing`](crate::buffer::BufferCfg::SpmcRing)) has none, so we fall back
    /// to the connection's drain cursor and return the **most recent** available
    /// value. Two consequences of that fallback: it *advances the shared drain
    /// cursor* (so `record.get` and `record.drain` interleave on one connection),
    /// and it yields `NotFound` until the ring produces a value *after* the cursor
    /// is first opened (a fresh broadcast reader starts at the tail). Use
    /// `record.drain` for a ring's full backlog.
    fn record_get(&mut self, name: &str) -> Result<Value, RpcError> {
        if let Some(v) = self.db.try_latest_as_json(name) {
            return Ok(v);
        }
        // Ring fallback: drain to the newest currently-available value (or NotFound).
        self.drain_values(name, usize::MAX)?
            .pop()
            .ok_or(RpcError::NotFound)
    }

    /// `record.drain`: return everything accumulated since the previous drain
    /// (capped by an optional `limit`), via the per-connection cursor.
    fn record_drain(&mut self, params: Value) -> Result<Value, RpcError> {
        let name = str_field(&params, "name").ok_or(RpcError::Internal)?;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX);
        let values = self.drain_values(&name, limit)?;
        let count = values.len();
        Ok(json!({ "record_name": name, "values": values, "count": count }))
    }

    /// Lazily open (on first call) the per-record drain cursor and read up to
    /// `limit` values accumulated since the previous read (oldest-first). Shared
    /// by [`record.drain`](Self::record_drain) and [`record.get`](Self::record_get)'s
    /// ring fallback, so both read from the same per-connection cursor.
    fn drain_values(&mut self, name: &str, limit: usize) -> Result<Vec<Value>, RpcError> {
        if !self.drain_readers.contains_key(name) {
            let id = self
                .db
                .inner()
                .resolve_str(name)
                .ok_or(RpcError::NotFound)?;
            let record = self.db.inner().storage(id).ok_or(RpcError::NotFound)?;
            // `subscribe_json` fails if the record was not configured with
            // `.with_remote_access()`.
            let reader = record.subscribe_json().map_err(map_db_err)?;
            self.drain_readers.insert(name.to_string(), reader);
        }

        let reader = self.drain_readers.get_mut(name).expect("inserted above");
        let mut values = Vec::new();
        while values.len() < limit {
            match reader.try_recv_json() {
                Ok(val) => values.push(val),
                Err(DbError::BufferEmpty) => break,
                // Ring overflowed since the last read — cursor resets; keep going.
                Err(DbError::BufferLagged { .. }) => continue,
                Err(_) => break,
            }
        }
        Ok(values)
    }

    /// `record.query`: resolve the persistence query handler registered in the
    /// db's `Extensions` (absent → not configured) and return its handler
    /// future.
    ///
    /// Deliberately **not** an `async fn`: an `async fn(&self)` future would
    /// capture `&self` across its await, forcing `AimxSession: Sync` — which the
    /// per-connection `drain_readers` (`Box<dyn _ + Send>`, not `Sync`) is not.
    /// Returning the (`'static`, `Send`) handler future lets the borrow of
    /// `self` end here; the caller awaits the owned future.
    #[allow(clippy::type_complexity)]
    fn record_query(
        &self,
        params: Value,
    ) -> Result<
        core::pin::Pin<
            Box<dyn core::future::Future<Output = Result<Value, String>> + Send + 'static>,
        >,
        RpcError,
    > {
        let handler = self
            .db
            .extensions()
            .get::<crate::remote::QueryHandlerFn>()
            .ok_or(RpcError::Internal)?;
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
        Ok(handler(crate::remote::QueryHandlerParams {
            name,
            limit,
            start,
            end,
        }))
    }

    /// Build the `Welcome` from the security policy + writable records.
    ///
    /// `writable_records` is derived from the policy directly and intersected with
    /// the records that actually exist, so the server never advertises a phantom key.
    fn welcome(&self) -> Value {
        let (permissions, writable_records) = match &self.config.security_policy {
            SecurityPolicy::ReadOnly => (vec!["read".to_string()], Vec::new()),
            SecurityPolicy::ReadWrite { writable_records } => {
                let existing: std::collections::HashSet<String> = self
                    .db
                    .list_records()
                    .into_iter()
                    .map(|m: RecordMetadata| m.record_key)
                    .collect();
                let writable = writable_records
                    .iter()
                    .filter(|name| existing.contains(name.as_str()))
                    .cloned()
                    .collect();
                (vec!["read".to_string(), "write".to_string()], writable)
            }
        };
        let welcome = WelcomeMessage {
            version: "2.0".to_string(),
            server: "aimdb".to_string(),
            permissions,
            writable_records,
            max_subscriptions: Some(self.config.max_subs_per_connection),
            authenticated: Some(false),
        };
        json!(welcome)
    }

    /// Deny unless the policy is ReadWrite and `name` is in its writable set.
    fn ensure_writable(&self, name: &str) -> Result<(), RpcError> {
        match &self.config.security_policy {
            SecurityPolicy::ReadWrite { writable_records } if writable_records.contains(name) => {
                Ok(())
            }
            _ => Err(RpcError::Denied),
        }
    }

    /// Deny under a ReadOnly policy (used by the `*.reset` admin methods).
    #[cfg(any(feature = "profiling", feature = "metrics"))]
    fn ensure_write_permission(&self) -> Result<(), RpcError> {
        match self.config.security_policy {
            SecurityPolicy::ReadOnly => Err(RpcError::Denied),
            SecurityPolicy::ReadWrite { .. } => Ok(()),
        }
    }
}

/// Serialize a JSON value into an owned record-value [`Payload`] (one serde pass
/// at the reply boundary, per doc 037 Decision 1).
fn to_payload(v: &Value) -> Payload {
    Payload::from(serde_json::to_vec(v).unwrap_or_default().as_slice())
}

/// Extract a string field from a params object.
fn str_field(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// Map a [`DbError`] onto the reshaped wire's coarse [`RpcError`] set.
fn map_db_err(e: DbError) -> RpcError {
    match e {
        DbError::RecordKeyNotFound { .. } | DbError::InvalidRecordId { .. } => RpcError::NotFound,
        DbError::PermissionDenied { .. } => RpcError::Denied,
        _ => RpcError::Internal,
    }
}
