//! Engine-based AimX client (Phase 3, client-first).
//!
//! Rebuilds the client on the shared session engine: a [`UdsDialer`] + the
//! symmetric [`AimxCodec`] drive [`run_client`], which owns the wire, the
//! request-id demux, and (optionally) reconnect. The public surface is the
//! cheap-clone [`ClientHandle`] plus typed convenience wrappers and
//! per-subscription [`futures::Stream`]s — a deliberate **break** from the old
//! synchronous [`crate::connection::AimxClient`] (`&mut self`, single global
//! `receive_event()` queue), which stays until the server port retires it.
//!
//! `run_client` is itself spawn-free (it returns a future for a runner to
//! drive); this convenience layer is a *client application*, so it drives the
//! engine on a `tokio::spawn`ed task held by [`AimxConnection`]. Dropping the
//! connection drops the handle, which stops the engine gracefully.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use aimdb_core::session::aimx::{AimxCodec, UdsDialer};
use aimdb_core::session::{run_client, BoxStream, ClientConfig, ClientHandle, Payload, RpcError};
use aimdb_tokio_adapter::TokioAdapter;

use crate::error::{ClientError, ClientResult};
use crate::protocol::{RecordMetadata, WelcomeMessage};

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
    /// Dial `socket_path`, start the engine, and complete the `hello` handshake,
    /// bounded by [`DEFAULT_CONNECT_TIMEOUT`].
    ///
    /// The handshake is a normal RPC (`call("hello", …) -> Welcome`) rather than
    /// a privileged frame — the reshaped wire's deliberate simplification. A dial
    /// failure surfaces here as the `hello` call failing (the engine runs with
    /// reconnect off so connect-time errors are prompt); a peer that accepts but
    /// never replies surfaces as a timeout (see [`connect_with_timeout`](Self::connect_with_timeout)).
    pub async fn connect(socket_path: impl AsRef<Path>) -> ClientResult<Self> {
        Self::connect_with_timeout(socket_path, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// Like [`connect`](Self::connect), but with an explicit handshake deadline.
    ///
    /// The deadline covers the whole handshake — dial *and* the `hello`/Welcome
    /// exchange — so a silent or unresponsive peer cannot block the caller
    /// indefinitely. On timeout (or any failure) the engine task is aborted so it
    /// does not linger blocked on a stalled connection.
    pub async fn connect_with_timeout(
        socket_path: impl AsRef<Path>,
        connect_timeout: Duration,
    ) -> ClientResult<Self> {
        let path = socket_path.as_ref();
        let dialer = UdsDialer::new(path);
        let config = ClientConfig {
            reconnect: false,
            sends_hello: false,
            ..ClientConfig::default()
        };
        let (handle, engine_fut) = run_client(dialer, AimxCodec, config, Arc::new(TokioAdapter));
        let engine = tokio::spawn(engine_fut);

        // Handshake-as-RPC: the server replies with its Welcome. Bounded so an
        // accepted-but-silent peer times out instead of hanging forever.
        let server_info = async {
            let hello = json!({ "client": "aimdb-client" });
            let reply = timeout(connect_timeout, handle.call("hello", to_payload(&hello)?))
                .await
                .map_err(|_| {
                    ClientError::connection_failed(
                        path.display().to_string(),
                        "handshake timed out",
                    )
                })?
                .map_err(|_| {
                    ClientError::connection_failed(
                        path.display().to_string(),
                        "handshake failed (engine could not reach server)",
                    )
                })?;
            from_payload::<WelcomeMessage>(&reply)
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

    /// Subscribe to a record's updates. Returns a stream of decoded JSON values;
    /// the engine routes events back by the request id it owns, so there is no
    /// `subscription_id` to track. Dropping the stream stops local delivery.
    pub fn subscribe(&self, name: &str) -> ClientResult<BoxStream<'static, serde_json::Value>> {
        let raw = self.handle.subscribe(name).map_err(rpc_err)?;
        // Decode each Payload into a JSON value; drop any that fail to parse.
        let decoded = raw.filter_map(|p| async move { serde_json::from_slice(&p).ok() });
        Ok(Box::pin(decoded))
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
