//! AimX server dispatch (`no_std + alloc`; features `connector-session` +
//! `remote`) — the method semantics of AimX remote access, served on the
//! shared session engine (`serve`/`run_session`).
//!
//! Reaches into core's `record.list` / JSON API (the `AnyRecord` JSON + metadata
//! methods), which are gated on `remote` and de-std'd alongside this
//! dispatch, so a board can serve a host over serial. A transport pairs this
//! dispatch with the generic
//! [`SessionServerConnector`](crate::session::SessionServerConnector) — see
//! `aimdb-uds-connector`'s `UdsServer`.
//!
//! The role is split in two:
//! - [`AimxDispatch`] — the shared half (one `Arc` per server): peer-only
//!   `authenticate` + an `open` factory.
//! - `AimxSession` — the per-connection half the engine owns by value, homing
//!   `record.drain`'s lazy per-record cursors (`drain_readers`).
//!
//! Param shapes follow the client ([`aimdb_client::AimxConnection`]):
//! `record.get`/`record.set` take `{name[, value]}`, `write` takes `{value}`.

use crate::session::events::{report, SessionEvent};

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;

use serde_json::{json, Value};

use futures_util::StreamExt;

use crate::buffer::JsonBufferReader;
use crate::remote::{AimxConfig, RecordMetadata, SecurityPolicy, WelcomeMessage, PROTOCOL_VERSION};
use crate::session::{
    is_wildcard, topic_matches, AuthError, BoxFut, BoxStream, Dispatch, Payload, PeerInfo,
    RpcError, Session, SessionCtx, SubUpdate,
};
use crate::{AimDb, DbError};

/// The shared AimX dispatch — `authenticate` (peer-only) + the `AimxSession`
/// factory. One `Arc<AimxDispatch>` is shared across every accepted connection.
pub struct AimxDispatch {
    db: Arc<AimDb>,
    config: Arc<AimxConfig>,
}

impl AimxDispatch {
    /// Build a dispatch over `db` with the given remote-access `config`.
    pub fn new(db: Arc<AimDb>, config: AimxConfig) -> Self {
        Self {
            db,
            config: Arc::new(config),
        }
    }
}

impl Dispatch for AimxDispatch {
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
struct AimxSession {
    db: Arc<AimDb>,
    config: Arc<AimxConfig>,
    /// Per-record drain readers, created lazily on first `record.drain`.
    drain_readers: HashMap<String, Box<dyn JsonBufferReader + Send>>,
}

/// Internal dispatch result. Most methods still build a JSON tree; hot record
/// paths can hand already-serialized JSON to the session engine.
enum DispatchReply {
    Value(Value),
    RawJson(Payload),
}

impl DispatchReply {
    fn into_payload(self) -> Payload {
        match self {
            Self::Value(value) => to_payload(&value),
            Self::RawJson(payload) => payload,
        }
    }
}

impl Session for AimxSession {
    /// `hello` opens every AimX session; a second one on a connection comes
    /// from a new client (a serial line never closes between clients).
    fn starts_session(&self, method: &str) -> bool {
        method == "hello"
    }

    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        Box::pin(async move {
            let params: Value = serde_json::from_slice(&params).unwrap_or(Value::Null);
            self.dispatch_call(method, params)
                .await
                .map(DispatchReply::into_payload)
        })
    }

    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, SubUpdate>, RpcError>> {
        // The engine owns the subscription lifecycle and the per-connection cap;
        // AimX has no async authorization, so this is a trivial wrapper.
        Box::pin(async move {
            if is_wildcard(topic) {
                return Ok(self.subscribe_wildcard(topic));
            }
            // Exact-topic fast path: resolve one key, untagged updates.
            let stream = crate::remote::stream::stream_record_updates(&self.db, topic)
                .map_err(map_db_err)?;
            Ok(
                Box::pin(stream.map(|(data, skipped)| SubUpdate::new(data).with_skipped(skipped)))
                    as BoxStream<'static, SubUpdate>,
            )
        })
    }

    fn snapshots(&mut self, topic: &str) -> Vec<(String, Payload)> {
        // Late-join state for wildcard subscriptions: the current value of every
        // matched record that has one. Exact-topic AimX subscribes stay
        // event-only (unchanged wire for UDS/serial/TCP clients).
        if !is_wildcard(topic) {
            return Vec::new();
        }
        self.matched_keys(topic)
            .into_iter()
            .filter_map(|key| {
                let value = self.db.try_latest_as_json(&key)?;
                Some((key, to_payload(&value)))
            })
            .collect()
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

impl AimxSession {
    /// Record keys matching a wildcard pattern. The record set is frozen at
    /// builder time, so matching once at subscribe time is complete — dynamic
    /// membership would require runtime registration.
    fn matched_keys(&self, pattern: &str) -> Vec<String> {
        self.db
            .list_records()
            .into_iter()
            .map(|m: RecordMetadata| m.record_key)
            .filter(|key| topic_matches(pattern, key))
            .collect()
    }

    /// One wildcard subscription fans in every matched record's update stream,
    /// each event tagged with the record that fired. Records without remote
    /// access are skipped — the subscription covers what can stream.
    fn subscribe_wildcard(&self, pattern: &str) -> BoxStream<'static, SubUpdate> {
        let mut streams: Vec<BoxStream<'static, SubUpdate>> = Vec::new();
        for key in self.matched_keys(pattern) {
            match crate::remote::stream::stream_record_updates(&self.db, &key) {
                Ok(stream) => {
                    let tag: Arc<str> = Arc::from(key.as_str());
                    streams.push(Box::pin(stream.map(move |(data, skipped)| {
                        SubUpdate::tagged(tag.clone(), data).with_skipped(skipped)
                    })));
                }
                Err(_e) => {
                    report(SessionEvent::WildcardSubscribeFailed {
                        pattern,
                        record: &key,
                        error: &_e,
                    });
                }
            }
        }
        // An empty match set yields a stream that ends immediately — with a
        // builder-frozen record set, "no matches now" is "no matches ever".
        Box::pin(futures_util::stream::select_all(streams))
    }

    /// Match the method and produce a tree or already-serialized JSON reply.
    async fn dispatch_call(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<DispatchReply, RpcError> {
        // Hot record read: hand already-serialized JSON straight to the engine,
        // skipping the `serde_json::Value` round-trip (main's clone-avoidance).
        if method == "record.get" {
            let name = str_field(&params, "name").ok_or(RpcError::NotFound)?;
            return self.record_get(&name).map(DispatchReply::RawJson);
        }
        // Identity (3.1): built as JSON bytes, the build-info payload spliced in.
        if method == "instance.info" {
            let reply = crate::remote::instance_info_reply(&self.db);
            return Ok(DispatchReply::RawJson(Payload::from(reply.as_slice())));
        }
        if method == "instance.post_link" {
            return crate::remote::post_link_chunk_reply(&self.db, &params)
                .map(|reply| DispatchReply::RawJson(Payload::from(reply.as_slice())))
                .map_err(map_post_link_err);
        }

        let value = match method {
            // Version gate: refuse an incompatible/absent client version at the
            // handshake rather than letting it trip over the new wire shapes.
            "hello" => self.hello(params),
            "record.list" => Ok(json!(self.db.list_records())),
            "record.set" => self.record_set(params),
            "record.drain" => self.record_drain(params),
            "record.query" => self
                .record_query(params)?
                .await
                .map_err(|_| RpcError::Internal),
            "graph.nodes" => Ok(json!(self.db.inner().dependency_graph().nodes)),
            "graph.edges" => Ok(json!(self.db.inner().dependency_graph().edges)),
            "graph.topo_order" => Ok(json!(self.db.inner().dependency_graph().topo_order())),
            #[cfg(feature = "observability")]
            "profiling.reset" => {
                self.ensure_write_permission()?;
                self.db.reset_stage_profiling();
                Ok(json!({ "reset": true }))
            }
            #[cfg(feature = "observability")]
            "buffer_metrics.reset" => {
                self.ensure_write_permission()?;
                self.db.reset_buffer_metrics();
                Ok(json!({ "reset": true }))
            }
            _ => Err(RpcError::NotFound),
        }?;

        Ok(DispatchReply::Value(value))
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
    /// ([`try_latest_as_json_bytes`](crate::AimDb::try_latest_as_json_bytes)).
    /// A ring
    /// ([`SpmcRing`](crate::buffer::BufferCfg::SpmcRing)) has none, so we fall back
    /// to the connection's drain cursor and return the **most recent** available
    /// value. Two consequences of that fallback: it *advances the shared drain
    /// cursor* (so `record.get` and `record.drain` interleave on one connection),
    /// and it yields `NotFound` until the ring produces a value *after* the cursor
    /// is first opened (a fresh broadcast reader starts at the tail). Use
    /// `record.drain` for a ring's full backlog.
    fn record_get(&mut self, name: &str) -> Result<Payload, RpcError> {
        if let Some(bytes) = self.db.try_latest_as_json_bytes(name) {
            return Ok(Payload::from(bytes));
        }
        // Ring fallback: drain to the newest currently-available value (or NotFound).
        self.drain_values(name, usize::MAX)?
            .pop()
            .map(|value| to_payload(&value))
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
            let reader = record
                .json_access()
                .ok_or_else(|| {
                    map_db_err(DbError::runtime_error(alloc::format!(
                        "Record '{name}' does not support JSON remote access"
                    )))
                })?
                .subscribe_json()
                .map_err(map_db_err)?;
            self.drain_readers.insert(name.to_string(), reader);
        }

        // Unreachable, but reported rather than asserted: this is the session
        // pump.
        let reader = self.drain_readers.get_mut(name).ok_or_else(|| {
            map_db_err(DbError::runtime_error(alloc::format!(
                "Record '{name}' drain reader vanished between insert and use"
            )))
        })?;
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
            .unwrap_or(crate::remote::QUERY_ALL_PATTERN)
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

    /// `hello`: the version-gated handshake.
    ///
    /// The client's declared `version` must be major-compatible with this
    /// server's [`PROTOCOL_VERSION`] ([`version_compatible`](crate::remote::version_compatible)).
    /// A pre-3.x — or version-less — client is refused **here** with
    /// [`RpcError::VersionMismatch`], rather than completing the handshake and
    /// letting the client trip over the new `reply`/`event` shapes on its first
    /// `record.query` — a hard refusal at `hello`.
    fn hello(&self, params: Value) -> Result<Value, RpcError> {
        let version = str_field(&params, "version").unwrap_or_default();
        if !crate::remote::version_compatible(&version) {
            report(SessionEvent::HandshakeVersionRejected {
                client_version: &version,
                server_version: PROTOCOL_VERSION,
            });
            return Err(RpcError::VersionMismatch);
        }
        Ok(self.welcome())
    }

    /// Build the `Welcome` from the security policy + writable records.
    ///
    /// `writable_records` is derived from the policy directly and intersected with
    /// the records that actually exist, so the server never advertises a phantom key.
    fn welcome(&self) -> Value {
        let (permissions, writable_records) = match &self.config.security_policy {
            SecurityPolicy::ReadOnly => (vec!["read".to_string()], Vec::new()),
            SecurityPolicy::ReadWrite { writable_records } => {
                let existing: hashbrown::HashSet<String> = self
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
            version: PROTOCOL_VERSION.to_string(),
            server: "aimdb".to_string(),
            permissions,
            writable_records,
            max_subscriptions: Some(self.config.max_subs_per_connection),
            authenticated: Some(false),
            instance_name: self.db.identity().name().map(String::from),
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
    #[cfg(feature = "observability")]
    fn ensure_write_permission(&self) -> Result<(), RpcError> {
        match self.config.security_policy {
            SecurityPolicy::ReadOnly => Err(RpcError::Denied),
            SecurityPolicy::ReadWrite { .. } => Ok(()),
        }
    }
}

/// Serialize a JSON value into an owned record-value [`Payload`] — one serde
/// pass at the reply boundary.
fn to_payload(v: &Value) -> Payload {
    Payload::from(serde_json::to_vec(v).unwrap_or_default().as_slice())
}

/// Extract a string field from a params object.
fn str_field(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// Map a [`PostLinkError`](crate::remote::PostLinkError) onto the wire's
/// error set.
fn map_post_link_err(e: crate::remote::PostLinkError) -> RpcError {
    match e {
        crate::remote::PostLinkError::NotWritten => RpcError::NotFound,
        crate::remote::PostLinkError::InvalidParams => RpcError::Internal,
    }
}

/// Map a [`DbError`] onto the reshaped wire's coarse [`RpcError`] set.
fn map_db_err(e: DbError) -> RpcError {
    match e {
        DbError::RecordKeyNotFound { .. } | DbError::InvalidRecordId { .. } => RpcError::NotFound,
        DbError::PermissionDenied { .. } => RpcError::Denied,
        _ => RpcError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{QueryHandlerFn, QUERY_ALL_PATTERN};
    use std::sync::Mutex;

    /// `record.query` with no `name` must reach the handler as the same pattern
    /// on every transport. It is a *record set*, not a formatting detail: under
    /// the MQTT grammar `*` spans exactly one dot-separated segment, so the old
    /// default silently dropped every nested record here while the WS connector
    /// returned them — the same request answering differently per link.
    ///
    /// Driven through the real dispatch (`Dispatch::open` → `Session::call`),
    /// since the defaulting lives in `record_query`, not at the wire edge. The
    /// WS dispatch defaults through the same constant, so the two cannot
    /// drift again.
    #[tokio::test]
    async fn an_omitted_query_name_spans_every_record() {
        static SEEN: Mutex<Option<String>> = Mutex::new(None);

        let handler: QueryHandlerFn = Box::new(|params| {
            *SEEN.lock().expect("uncontended") = Some(params.name.clone());
            Box::pin(async { Ok(json!({ "records": [], "total": 0 })) })
        });

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(crate::executor::test_support::NoopRuntimeOps));
        builder.extensions_mut().insert(handler);
        let (db, _runner) = builder.build().await.expect("build db");

        let dispatch = AimxDispatch::new(Arc::new(db), AimxConfig::uds_default());
        let mut session = dispatch.open(&SessionCtx::default());
        session
            .call("record.query", to_payload(&json!({})))
            .await
            .expect("the query resolves");

        assert_eq!(
            SEEN.lock().expect("uncontended").as_deref(),
            Some(QUERY_ALL_PATTERN),
            "an omitted name must span every record, at every depth"
        );
    }
    #[cfg(feature = "std")]
    #[tokio::test]
    async fn protocol_and_wildcard_failures_are_typed() {
        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(crate::executor::test_support::NoopRuntimeOps));
        // Listed metadata, but no JSON access: wildcard subscribe must skip it.
        builder.configure::<u32>("plain", |reg| {
            reg.buffer_cfg(crate::buffer::BufferCfg::SingleLatest);
        });
        let (db, _runner) = builder.build().await.expect("build db");
        let dispatch = AimxDispatch::new(Arc::new(db), AimxConfig::uds_default());
        let mut session = dispatch.open(&SessionCtx::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer = {
            let seen = seen.clone();
            Arc::new(move |event: &SessionEvent<'_>| {
                let tag = match event {
                    SessionEvent::HandshakeVersionRejected {
                        client_version,
                        server_version,
                    } => {
                        assert_eq!(
                            (*client_version, *server_version),
                            ("0.0.0", PROTOCOL_VERSION)
                        );
                        "version"
                    }
                    SessionEvent::WildcardSubscribeFailed {
                        pattern,
                        record,
                        error,
                    } => {
                        assert_eq!((*pattern, *record), ("#", "plain"));
                        assert!(matches!(error, DbError::RuntimeError { .. }));
                        "wildcard"
                    }
                    other => panic!("unexpected event: {other:?}"),
                };
                seen.lock().unwrap().push(tag);
            })
        };
        crate::session::events::capture::future(observer, async {
            assert_eq!(
                session
                    .call("hello", to_payload(&json!({ "version": "0.0.0" })))
                    .await,
                Err(RpcError::VersionMismatch)
            );
            let mut stream = session.subscribe("#").await.expect("wildcard stream");
            assert!(stream.next().await.is_none());
        })
        .await;
        assert_eq!(*seen.lock().unwrap(), ["version", "wildcard"]);
    }

    // ── instance.info / instance.post_link (3.1) ─────────────────────────

    /// The post-link region a test section carries.
    enum Region<'a> {
        None,
        Erased,
        Written(&'a [u8]),
        /// A length past the region's end.
        Malformed,
    }

    /// A format-1 section around `payload`, by hand, leaked as `section!()`.
    fn section(payload: &[u8], region: Region<'_>) -> &'static [u8] {
        let region_len = match region {
            Region::None => 0,
            Region::Written(data) => 4 + data.len(),
            Region::Erased | Region::Malformed => 16,
        };
        let region_off = (32 + payload.len()).div_ceil(8) * 8;
        let total = region_off + region_len;
        let mut b = vec![0u8; total];
        b[..8].copy_from_slice(&aimdb_build_info::MAGIC);
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[10..12].copy_from_slice(&32u16.to_le_bytes());
        b[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        b[16..20].copy_from_slice(&(region_off as u32).to_le_bytes());
        b[20..24].copy_from_slice(&(region_len as u32).to_le_bytes());
        b[24..28].copy_from_slice(&(total as u32).to_le_bytes());
        b[32..32 + payload.len()].copy_from_slice(payload);
        b[region_off..].fill(0xFF);
        match region {
            Region::Written(data) => {
                b[region_off..region_off + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
                b[region_off + 4..].copy_from_slice(data);
            }
            Region::Malformed => {
                b[region_off..region_off + 4].copy_from_slice(&13u32.to_le_bytes());
            }
            Region::None | Region::Erased => {}
        }
        Box::leak(b.into_boxed_slice())
    }

    async fn session_for(builder: crate::AimDbBuilder) -> Box<dyn Session> {
        let (db, _runner) = builder
            .runtime(Arc::new(crate::executor::test_support::NoopRuntimeOps))
            .build()
            .await
            .expect("build db");
        AimxDispatch::new(Arc::new(db), AimxConfig::uds_default()).open(&SessionCtx::default())
    }

    async fn call_raw(
        session: &mut Box<dyn Session>,
        method: &str,
        params: &[u8],
    ) -> Result<Vec<u8>, RpcError> {
        session
            .call(method, Payload::from(params))
            .await
            .map(|p| p.to_vec())
    }

    async fn call(
        session: &mut Box<dyn Session>,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let reply = call_raw(session, method, &serde_json::to_vec(&params).unwrap()).await?;
        Ok(serde_json::from_slice(&reply).expect("reply is JSON"))
    }

    const PAYLOAD: &[u8] =
        br#"{"schema":1,"name":"boiler-gateway","version":"1.4.0","ext":{"board":"h563"}}"#;

    #[tokio::test]
    async fn instance_info_without_identity() {
        let mut session = session_for(crate::AimDbBuilder::new()).await;
        let reply = call(&mut session, "instance.info", Value::Null)
            .await
            .unwrap();
        assert_eq!(
            reply,
            json!({
                "aimdb_version": env!("CARGO_PKG_VERSION"),
                "instance": { "name": null, "uid": null },
                "build": null,
            })
        );
        let welcome = call(
            &mut session,
            "hello",
            json!({ "version": PROTOCOL_VERSION }),
        )
        .await
        .unwrap();
        assert!(welcome.get("instance_name").is_none(), "{welcome}");
        assert_eq!(
            call(&mut session, "instance.post_link", json!({})).await,
            Err(RpcError::NotFound)
        );
    }

    #[tokio::test]
    async fn instance_info_with_identity() {
        let mut session = session_for(
            crate::AimDbBuilder::new()
                .instance_name("gateway-3")
                .instance_uid(&[0x00, 0x35, 0x00, 0x2e, 0xab])
                .build_info(section(PAYLOAD, Region::Written(b"signed"))),
        )
        .await;
        let reply = call_raw(&mut session, "instance.info", b"null")
            .await
            .unwrap();
        // The payload is spliced in byte for byte.
        let text = core::str::from_utf8(&reply).unwrap();
        assert!(
            text.contains(core::str::from_utf8(PAYLOAD).unwrap()),
            "{text}"
        );
        let reply: crate::remote::InstanceInfoReply = serde_json::from_slice(&reply).unwrap();
        assert_eq!(reply.instance.name.as_deref(), Some("gateway-3"));
        assert_eq!(reply.instance.uid.as_deref(), Some("0035002eab"));
        let build = reply.build.expect("build");
        assert_eq!(build.format, 1);
        assert_eq!(build.info["ext"]["board"], "h563");
        assert_eq!(
            build.post_link,
            Some(crate::remote::PostLinkReply::Written { len: 6 })
        );

        let welcome = call(
            &mut session,
            "hello",
            json!({ "version": PROTOCOL_VERSION }),
        )
        .await
        .unwrap();
        assert_eq!(welcome["instance_name"], "gateway-3");
    }

    #[tokio::test]
    async fn instance_info_reports_each_region_state() {
        use crate::remote::PostLinkReply;
        for (region, expected) in [
            (Region::None, Value::Null),
            (Region::Erased, json!({ "state": "erased" })),
            (
                Region::Written(b""),
                json!({ "state": "written", "len": 0 }),
            ),
            (Region::Malformed, json!({ "state": "malformed" })),
        ] {
            let mut session =
                session_for(crate::AimDbBuilder::new().build_info(section(b"{}", region))).await;
            let reply = call(&mut session, "instance.info", json!({}))
                .await
                .unwrap();
            assert_eq!(reply["build"]["post_link"], expected);
            assert_eq!(reply["build"]["info"], json!({}));
            // Every state but `written` has nothing to read.
            let chunk = call(&mut session, "instance.post_link", json!({})).await;
            match serde_json::from_value::<Option<PostLinkReply>>(expected).unwrap() {
                Some(PostLinkReply::Written { .. }) => assert!(chunk.is_ok()),
                _ => assert_eq!(chunk, Err(RpcError::NotFound)),
            }
        }
    }

    async fn chunk(session: &mut Box<dyn Session>, params: Value) -> Value {
        call(session, "instance.post_link", params).await.unwrap()
    }

    #[tokio::test]
    async fn post_link_chunks() {
        let data: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| alloc::format!("{b:02x}"))
                .collect::<String>()
        };
        let mut session = session_for(
            crate::AimDbBuilder::new().build_info(section(b"{}", Region::Written(&data))),
        )
        .await;

        // Defaults: offset 0, 1 KiB; `null` params equal `{}`.
        for params in [Value::Null, json!({})] {
            assert_eq!(
                chunk(&mut session, params).await,
                json!({ "offset": 0, "total": 1500, "data": hex(&data[..1024]) })
            );
        }
        // The rest, with `len` over the maximum clamped.
        assert_eq!(
            chunk(&mut session, json!({ "offset": 1024, "len": u32::MAX })).await,
            json!({ "offset": 1024, "total": 1500, "data": hex(&data[1024..]) })
        );
        assert_eq!(
            chunk(&mut session, json!({ "offset": 10, "len": 3 })).await,
            json!({ "offset": 10, "total": 1500, "data": hex(&data[10..13]) })
        );
        // `len: 0` and offsets at or past the end: no data, `total` still set.
        for params in [
            json!({ "len": 0 }),
            json!({ "offset": 1500 }),
            json!({ "offset": u32::MAX, "len": u32::MAX }),
        ] {
            let reply = chunk(&mut session, params.clone()).await;
            assert_eq!(
                (reply["data"].as_str(), reply["total"].as_u64()),
                (Some(""), Some(1500)),
                "{params}"
            );
        }
    }

    #[tokio::test]
    async fn post_link_rejects_malformed_params_before_the_region() {
        let written = section(b"{}", Region::Written(b"x"));
        for builder in [
            crate::AimDbBuilder::new().build_info(written),
            // No section: still `internal`, not `not_found`.
            crate::AimDbBuilder::new(),
        ] {
            let mut session = session_for(builder).await;
            for params in [
                json!({ "offset": -1 }),
                json!({ "offset": 1.5 }),
                json!({ "offset": "0" }),
                json!({ "offset": null }),
                json!({ "offset": u64::from(u32::MAX) + 1 }),
                json!({ "len": -1 }),
                json!({ "len": 2.0 }),
                json!({ "len": "8" }),
                json!({ "len": null }),
                json!({ "len": u64::from(u32::MAX) + 1 }),
                json!([0, 8]),
                json!("offset"),
                json!(7),
            ] {
                assert_eq!(
                    call(&mut session, "instance.post_link", params.clone()).await,
                    Err(RpcError::Internal),
                    "{params}"
                );
            }
        }
    }

    /// The largest identity the builder accepts stays within
    /// `INSTANCE_INFO_REPLY_MAX`.
    #[tokio::test]
    async fn worst_case_instance_info_fits_the_bound() {
        let mut payload = vec![b' '; aimdb_build_info::PAYLOAD_MAX];
        payload[0] = b'{';
        payload[aimdb_build_info::PAYLOAD_MAX - 1] = b'}';
        // Control characters escape to `\u00XX`: six bytes each.
        let name: String = core::iter::repeat_n('\u{1}', crate::identity::NAME_MAX).collect();
        let mut session = session_for(
            crate::AimDbBuilder::new()
                .instance_name(name)
                .instance_uid(&[0xff; crate::identity::UID_MAX])
                .build_info(section(&payload, Region::Erased)),
        )
        .await;
        let reply = call_raw(&mut session, "instance.info", b"{}")
            .await
            .unwrap();
        assert!(
            reply.len() <= crate::remote::INSTANCE_INFO_REPLY_MAX,
            "{} bytes",
            reply.len()
        );
        assert!(!reply.contains(&b'\n') && !reply.contains(&b'\r'));
    }
}
