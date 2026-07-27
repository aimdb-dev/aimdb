//! Engine-based AimX client.
//!
//! The client rides the shared session engine: a [`UdsDialer`] + the symmetric
//! [`AimxCodec`] drive [`run_client`], which owns the wire, the request-id
//! demux, and (optionally) reconnect. The public surface is the cheap-clone
//! [`ClientHandle`] plus typed convenience wrappers and per-subscription
//! [`futures::Stream`]s.
//!
//! `run_client` is itself spawn-free (it returns a future for a runner to
//! drive); this convenience layer is a *client application*, so it drives the
//! engine on a `tokio::spawn`ed task held by [`AimxConnection`]. Dropping the
//! connection drops the handle, which stops the engine gracefully.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{
    run_client, BoxStream, ClientConfig, ClientHandle, Dialer, Payload, RpcError,
};
use aimdb_tokio_adapter::TokioAdapter;

use crate::error::{ClientError, ClientResult};
use crate::protocol::{version_compatible, RecordMetadata, WelcomeMessage, PROTOCOL_VERSION};

/// Default deadline for the connect handshake (dial + `hello`/Welcome). Bounds
/// the case where a peer accepts the socket but never replies — the engine has
/// no handshake timeout of its own, so the wait would otherwise be unbounded.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Response from a `record.drain` call: the values accumulated since the
/// previous drain for this connection's per-record cursor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainResponse {
    /// Echo of the queried record name.
    pub record_name: String,
    /// Chronologically ordered values (raw JSON, as written by the producer).
    pub values: Vec<serde_json::Value>,
    /// Number of values returned.
    pub count: usize,
}

/// One decoded update from a subscription, carrying the delivery gap that
/// preceded it.
///
/// Yielded by [`AimxConnection::subscribe_updates`]; the value-only
/// [`subscribe`](AimxConnection::subscribe) /
/// [`subscribe_with_topics`](AimxConnection::subscribe_with_topics) streams
/// project this down to the parts they expose.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordUpdate {
    /// Concrete record that fired. `Some` on wildcard subscriptions, which fan
    /// in many records under one subscription; `None` when the server left the
    /// event untagged (exact-topic subscribe).
    pub topic: Option<String>,
    /// The decoded record value.
    pub value: serde_json::Value,
    /// Updates lost between the previous delivered update and this one — `0`
    /// when delivery was lossless. Non-zero means the server-side buffer
    /// overran a slow consumer (or the payload was undecodable), so the value
    /// stream has a hole immediately before this item.
    pub skipped: u64,
}

impl RecordUpdate {
    /// Whether updates were lost immediately before this one.
    pub fn has_gap(&self) -> bool {
        self.skipped > 0
    }
}

/// A live connection to an AimDB instance over the shared session engine.
///
/// Holds the cheap-clone [`ClientHandle`] (use [`handle`](Self::handle) to issue
/// raw `call`/`subscribe`/`write`) and the driven engine task. Typed wrappers
/// cover the common AimX methods.
pub struct AimxConnection {
    handle: ClientHandle,
    engine: JoinHandle<()>,
    server_info: WelcomeMessage,
}

impl AimxConnection {
    /// Resolve `endpoint` to a transport, start the engine, and complete the
    /// `hello` handshake, bounded by [`DEFAULT_CONNECT_TIMEOUT`].
    ///
    /// `endpoint` is a `scheme://` URL — `unix://PATH` / `uds://PATH`,
    /// `serial://DEVICE?baud=N` — or a bare path (the `unix://` shorthand). The
    /// scheme picks the transport via [`crate::endpoint::dial`]; an unknown scheme
    /// (or one whose transport isn't compiled in) fails before the engine starts.
    pub async fn connect(endpoint: &str) -> ClientResult<Self> {
        Self::connect_with_timeout(endpoint, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// Like [`connect`](Self::connect), but with an explicit handshake deadline.
    pub async fn connect_with_timeout(
        endpoint: &str,
        connect_timeout: Duration,
    ) -> ClientResult<Self> {
        let dialer = crate::endpoint::dial(endpoint)?;
        Self::spin_up(dialer, endpoint, connect_timeout).await
    }

    /// Connect over an explicit [`Dialer`], bypassing endpoint resolution — for
    /// transports the resolver doesn't cover, or a pre-built dialer.
    pub async fn connect_over(dialer: impl Dialer + 'static) -> ClientResult<Self> {
        Self::connect_over_with_timeout(dialer, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// Like [`connect_over`](Self::connect_over), with an explicit handshake deadline.
    pub async fn connect_over_with_timeout(
        dialer: impl Dialer + 'static,
        connect_timeout: Duration,
    ) -> ClientResult<Self> {
        Self::spin_up(dialer, "remote peer", connect_timeout).await
    }

    /// Spin up the engine over `dialer` and complete the `hello` handshake.
    ///
    /// The handshake is a normal RPC (`call("hello", …) -> Welcome`) rather than
    /// a privileged frame — the reshaped wire's deliberate simplification. The
    /// `connect_timeout` covers the whole handshake (dial *and* the Welcome
    /// exchange), so a silent or unresponsive peer can't block the caller; on any
    /// failure the engine task is aborted so it doesn't linger blocked. `label`
    /// names the endpoint in error messages.
    async fn spin_up(
        dialer: impl Dialer + 'static,
        label: &str,
        connect_timeout: Duration,
    ) -> ClientResult<Self> {
        let config = ClientConfig {
            reconnect: false,
            sends_hello: false,
            ..ClientConfig::default()
        };
        let (handle, engine_fut) = run_client(dialer, AimxCodec, config, Arc::new(TokioAdapter));
        let engine = tokio::spawn(engine_fut);

        let server_info = async {
            let hello = json!({ "client": "aimdb-client", "version": PROTOCOL_VERSION });
            let reply = timeout(connect_timeout, handle.call("hello", to_payload(&hello)?))
                .await
                .map_err(|_| ClientError::connection_failed(label, "handshake timed out"))?
                .map_err(|e| match e {
                    // The server refused our declared version — report it as
                    // such, not as an unreachable-engine failure.
                    RpcError::VersionMismatch => ClientError::connection_failed(
                        label,
                        format!(
                            "protocol version mismatch: server rejected client version {PROTOCOL_VERSION}"
                        ),
                    ),
                    _ => ClientError::connection_failed(
                        label,
                        "handshake failed (engine could not reach server)",
                    ),
                })?;
            let welcome = from_payload::<WelcomeMessage>(&reply)?;
            // Reverse gate: a pre-3.x server ignores our `version` and completes
            // the handshake with an old-shaped `welcome`. Detect that here so a
            // new client fails fast instead of tripping over the old reply shapes.
            if !version_compatible(&welcome.version) {
                return Err(ClientError::connection_failed(
                    label,
                    format!(
                        "protocol version mismatch: server speaks {}, client speaks {PROTOCOL_VERSION}",
                        welcome.version
                    ),
                ));
            }
            Ok(welcome)
        }
        .await;

        match server_info {
            Ok(server_info) => Ok(Self {
                handle,
                engine,
                server_info,
            }),
            Err(e) => {
                // Don't leave the engine task blocked on a stalled dial/connection.
                engine.abort();
                Err(e)
            }
        }
    }

    /// The raw engine handle — `call` / `subscribe` / `write` for methods the
    /// typed wrappers below don't cover.
    pub fn handle(&self) -> &ClientHandle {
        &self.handle
    }

    /// The server's `Welcome` (permissions, writable records) from the handshake.
    pub fn server_info(&self) -> &WelcomeMessage {
        &self.server_info
    }

    /// List all registered records.
    pub async fn list_records(&self) -> ClientResult<Vec<RecordMetadata>> {
        let reply = self.call("record.list", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Get a record's current value.
    ///
    /// For a `SingleLatest`/state record this is a non-destructive read. A ring
    /// (`SpmcRing`) has no canonical latest, so the server returns the most recent
    /// value from this connection's drain cursor — which **advances that cursor**
    /// (interleaving with [`drain_record`](Self::drain_record)) and is empty until
    /// the ring produces a value after the connection first reads it. Prefer
    /// [`drain_record`](Self::drain_record) for ring/history records.
    pub async fn get_record(&self, name: &str) -> ClientResult<serde_json::Value> {
        let reply = self
            .call("record.get", to_payload(&json!({ "name": name }))?)
            .await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Set a record's value (RPC; awaits the server's reply).
    pub async fn set_record(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        let reply = self
            .call(
                "record.set",
                to_payload(&json!({ "name": name, "value": value }))?,
            )
            .await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Subscribe to a record or topic pattern, yielding the full
    /// [`RecordUpdate`] — decoded value, per-record topic, **and** the
    /// [`skipped`](RecordUpdate::skipped) gap count.
    ///
    /// This is the loss-aware form: the engine tracks how many updates the
    /// server-side buffer dropped between deliveries, and a consumer that cares
    /// about completeness (a watcher, a mirror, an archiver) must observe it.
    /// [`subscribe`](Self::subscribe) and
    /// [`subscribe_with_topics`](Self::subscribe_with_topics) are value-only
    /// views over this stream and silently discard the gap.
    ///
    /// Wildcards (`#`, `*`) are supported: one subscription fans in every
    /// matching record, each update tagged with the record that fired. A
    /// terminal rejection item ends the stream. Dropping the stream stops local
    /// delivery.
    pub fn subscribe_updates(
        &self,
        pattern: &str,
    ) -> ClientResult<BoxStream<'static, RecordUpdate>> {
        let raw = self.handle.subscribe(pattern).map_err(rpc_err)?;
        Ok(decode_updates(raw))
    }

    /// Subscribe to a record's updates. Returns a stream of decoded JSON values;
    /// the engine routes events back by the request id it owns, so there is no
    /// `subscription_id` to track. Dropping the stream stops local delivery.
    ///
    /// Delivery gaps are **not** visible here — use
    /// [`subscribe_updates`](Self::subscribe_updates) to observe them. For the
    /// per-record topic (wildcard subscriptions), see
    /// [`subscribe_with_topics`](Self::subscribe_with_topics).
    pub fn subscribe(&self, name: &str) -> ClientResult<BoxStream<'static, serde_json::Value>> {
        Ok(Box::pin(self.subscribe_updates(name)?.map(|u| u.value)))
    }

    /// Subscribe to a topic pattern (wildcards supported: `#`, `*`), yielding
    /// `(topic, value)` pairs. One wildcard subscription fans in every matching
    /// record; each update names the record that fired. The topic is `None`
    /// only when the server left the event untagged (exact-topic subscribe).
    ///
    /// Delivery gaps are **not** visible here — use
    /// [`subscribe_updates`](Self::subscribe_updates) to observe them.
    pub fn subscribe_with_topics(
        &self,
        pattern: &str,
    ) -> ClientResult<BoxStream<'static, (Option<String>, serde_json::Value)>> {
        Ok(Box::pin(
            self.subscribe_updates(pattern)?.map(|u| (u.topic, u.value)),
        ))
    }

    /// Fire-and-forget write to a record (no reply; routes through the server's
    /// producer/arbiter path — single-writer-per-key stays intact).
    pub fn write_record(&self, name: &str, value: serde_json::Value) -> ClientResult<()> {
        self.handle
            .write(name, to_payload(&json!({ "value": value }))?)
            .map_err(rpc_err)
    }

    /// Drain all values accumulated since the previous drain of `name` (a
    /// destructive read against this connection's per-record cursor).
    pub async fn drain_record(&self, name: &str) -> ClientResult<DrainResponse> {
        let reply = self
            .call("record.drain", to_payload(&json!({ "name": name }))?)
            .await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Drain at most `limit` values from `name`.
    pub async fn drain_record_with_limit(
        &self,
        name: &str,
        limit: u32,
    ) -> ClientResult<DrainResponse> {
        let reply = self
            .call(
                "record.drain",
                to_payload(&json!({ "name": name, "limit": limit }))?,
            )
            .await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Run a persistence query (requires the server's `with_persistence()`).
    pub async fn query(&self, params: serde_json::Value) -> ClientResult<serde_json::Value> {
        let reply = self.call("record.query", to_payload(&params)?).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// All nodes in the dependency graph.
    pub async fn graph_nodes(&self) -> ClientResult<Vec<serde_json::Value>> {
        let reply = self.call("graph.nodes", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// All edges in the dependency graph.
    pub async fn graph_edges(&self) -> ClientResult<Vec<serde_json::Value>> {
        let reply = self.call("graph.edges", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Record keys in topological order.
    pub async fn graph_topo_order(&self) -> ClientResult<Vec<String>> {
        let reply = self.call("graph.topo_order", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Reset stage-profiling counters (server built with `profiling`; needs write
    /// permission).
    pub async fn reset_stage_profiling(&self) -> ClientResult<serde_json::Value> {
        let reply = self.call("profiling.reset", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Reset buffer-metrics counters (server built with `metrics`; needs write
    /// permission).
    pub async fn reset_buffer_metrics(&self) -> ClientResult<serde_json::Value> {
        let reply = self.call("buffer_metrics.reset", null_payload()).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Issue a raw RPC and map a transport/engine failure to [`ClientError`].
    async fn call(&self, method: &str, params: Payload) -> ClientResult<Payload> {
        self.handle.call(method, params).await.map_err(rpc_err)
    }
}

impl Drop for AimxConnection {
    fn drop(&mut self) {
        // Dropping `handle` already stops the engine; abort is just promptness.
        self.engine.abort();
    }
}

/// Decode a raw engine subscription stream into [`RecordUpdate`]s.
///
/// Each update's payload is decoded into a JSON value; a terminal rejection item
/// ends the stream. An undecodable payload never reaches the caller, so it is
/// itself a gap: its own `skipped` count plus one for the dropped item are
/// carried into the next delivered update rather than lost with it.
fn decode_updates(
    raw: BoxStream<'static, Result<aimdb_core::session::SubUpdate, RpcError>>,
) -> BoxStream<'static, RecordUpdate> {
    let decoded = raw
        .scan(0u64, |carry, item| {
            let update = match item {
                Ok(u) => match serde_json::from_slice(&u.data) {
                    Ok(value) => {
                        let skipped = *carry + u.skipped;
                        *carry = 0;
                        Some(RecordUpdate {
                            topic: u.topic.as_deref().map(String::from),
                            value,
                            skipped,
                        })
                    }
                    Err(_) => {
                        *carry += u.skipped + 1;
                        None
                    }
                },
                // Terminal rejection: end the stream.
                Err(_) => return futures::future::ready(None),
            };
            futures::future::ready(Some(update))
        })
        .filter_map(futures::future::ready);
    Box::pin(decoded)
}

/// Serialize a value into a record-value [`Payload`].
fn to_payload<T: Serialize>(value: &T) -> ClientResult<Payload> {
    Ok(Payload::from(serde_json::to_vec(value)?.as_slice()))
}

/// The JSON literal `null` as a [`Payload`] — for methods that take no params.
fn null_payload() -> Payload {
    Payload::from(&b"null"[..])
}

/// Decode a [`Payload`] into a typed value.
fn from_payload<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> ClientResult<T> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Map an engine [`RpcError`] onto a [`ClientError`].
fn rpc_err(e: RpcError) -> ClientError {
    match e {
        RpcError::NotFound => {
            ClientError::server_error("not_found", "method or record not found", None)
        }
        RpcError::Denied => ClientError::server_error("denied", "permission denied", None),
        // `Internal` today, plus any future non-exhaustive variant.
        _ => ClientError::server_error("internal", "engine/transport failure", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::session::SubUpdate;
    use serde_json::json;

    /// Build a raw engine stream from a fixed list of subscription items.
    fn raw_stream(
        items: Vec<Result<SubUpdate, RpcError>>,
    ) -> BoxStream<'static, Result<SubUpdate, RpcError>> {
        Box::pin(futures::stream::iter(items))
    }

    fn ok_update(payload: &str, skipped: u64) -> Result<SubUpdate, RpcError> {
        Ok(SubUpdate::new(Payload::from(payload.as_bytes())).with_skipped(skipped))
    }

    #[tokio::test]
    async fn gap_counts_reach_the_caller() {
        let raw = raw_stream(vec![ok_update(r#"{"n":1}"#, 0), ok_update(r#"{"n":2}"#, 7)]);
        let out: Vec<_> = decode_updates(raw).collect().await;

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].value, json!({ "n": 1 }));
        assert_eq!(out[0].skipped, 0);
        assert!(!out[0].has_gap());
        assert_eq!(out[1].value, json!({ "n": 2 }));
        assert_eq!(out[1].skipped, 7, "the engine's gap count is preserved");
        assert!(out[1].has_gap());
    }

    #[tokio::test]
    async fn undecodable_payloads_fold_into_the_next_gap() {
        // A payload the caller never sees is itself a loss: it (and the gap it
        // reported) must show up on the next delivered update, not vanish.
        let raw = raw_stream(vec![
            ok_update(r#"{"n":1}"#, 0),
            ok_update("not json", 3),
            ok_update("also not json", 0),
            ok_update(r#"{"n":2}"#, 1),
        ]);
        let out: Vec<_> = decode_updates(raw).collect().await;

        assert_eq!(out.len(), 2, "undecodable items are not delivered");
        assert_eq!(out[1].value, json!({ "n": 2 }));
        // 3 reported + 1 dropped item + 1 dropped item + 1 reported = 6
        assert_eq!(out[1].skipped, 6);
    }

    #[tokio::test]
    async fn topics_ride_along_and_rejection_ends_the_stream() {
        let raw = raw_stream(vec![
            Ok(
                SubUpdate::tagged(Arc::from("temp.vienna"), Payload::from(&b"1"[..]))
                    .with_skipped(2),
            ),
            Err(RpcError::Denied),
            ok_update("99", 0), // past the terminal item — never delivered
        ]);
        let out: Vec<_> = decode_updates(raw).collect().await;

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].topic.as_deref(), Some("temp.vienna"));
        assert_eq!(out[0].value, json!(1));
        assert_eq!(out[0].skipped, 2);
    }
}
