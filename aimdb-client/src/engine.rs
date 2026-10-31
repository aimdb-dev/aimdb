//! Engine-based AimX client.
//!
//! The client rides the shared session engine: a `UdsDialer` (unlinked — it
//! exists only behind `transport-uds`) + the symmetric
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
use serde::Serialize;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{
    run_client, BoxStream, ClientConfig, ClientHandle, Dialer, Payload, RpcError,
};
use aimdb_tokio_adapter::TokioAdapter;

use crate::error::{ClientError, ClientResult};
use crate::protocol::{
    version_compatible, InstanceInfoReply, PostLinkChunk, RecordMetadata, WelcomeMessage,
    POST_LINK_CHUNK_MAX, PROTOCOL_VERSION,
};
use aimdb_core::{Stamp, Stamped};

/// Default deadline for the connect handshake (dial + `hello`/Welcome). Bounds
/// the case where a peer accepts the socket but never replies — the engine has
/// no handshake timeout of its own, so the wait would otherwise be unbounded.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Response from a `record.drain` call: the values accumulated since the
/// previous drain for this connection's per-record cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct DrainResponse {
    /// Echo of the queried record name.
    pub record_name: String,
    /// Chronologically ordered values (raw JSON, as written by the producer),
    /// each with the stamp of its write.
    pub values: Vec<Stamped<serde_json::Value>>,
    /// Number of values returned.
    pub count: usize,
}

/// One update from a subscription: the record value, plus the delivery metadata
/// the engine tracks alongside it.
///
/// One inbound frame yields exactly one of these, so [`skipped`](Self::skipped)
/// and [`snapshot_end`](Self::snapshot_end) always describe the frame they
/// arrive on.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordUpdate {
    /// Concrete record that fired. `Some` on wildcard subscriptions, which fan
    /// in many records under one subscription; `None` when the server left the
    /// event untagged (exact-topic subscribe).
    pub topic: Option<String>,
    /// The decoded record value, or `None` if the payload did not decode — a
    /// malformed peer, since the server serializes record values as JSON.
    ///
    /// An undecodable frame is still delivered, so that
    /// [`skipped`](Self::skipped) and [`snapshot_end`](Self::snapshot_end) stay
    /// attached to the frame they describe, and so one bad record cannot end a
    /// wildcard stream that fans in many.
    pub value: Option<serde_json::Value>,
    /// When the value was written, in nanoseconds since the server instance
    /// started (AimX 4.0 `ts`). Only comparable within one instance lifetime:
    /// reset comparisons after a reconnect.
    pub ts: Stamp,
    /// Updates lost between the previous delivered update and this one — `0`
    /// when delivery was lossless. Non-zero means the server-side buffer
    /// overran a slow consumer, so the value stream has a hole immediately
    /// before this item.
    pub skipped: u64,
    /// Final update of a late-join snapshot burst: every update after it is a
    /// live event. A burst that overran the sink carries its whole loss in
    /// [`skipped`](Self::skipped) here — the engine reserves a slot for this
    /// update, so an overrun cannot cost you the marker.
    ///
    /// **Its absence is not a signal**, and must never be waited on. The marker
    /// rides the last snapshot's frame, so a burst that produced none never
    /// sets it: nothing matched, nothing has a value yet, an exact-topic
    /// subscribe, or a tail that failed to encode. Only the last of those is a
    /// real loss, and [`skipped`](Self::skipped) reports it.
    pub snapshot_end: bool,
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
        let ctx = aimdb_core::RuntimeContext::new(Arc::new(TokioAdapter));
        let (handle, engine_fut) = run_client(dialer, AimxCodec, config, ctx);
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
            // Reverse gate: a server of another major (an older one ignores our
            // `version`) completes the handshake with its own `welcome`. Detect
            // that here so a new client fails fast instead of tripping over old
            // reply shapes, such as 3.x values without `ts`.
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
    ///
    /// The value comes with the stamp of its write; a reply without `ts` is a
    /// [`ClientError::Protocol`].
    pub async fn get_record(&self, name: &str) -> ClientResult<Stamped<serde_json::Value>> {
        let reply = self
            .call("record.get", to_payload(&json!({ "name": name }))?)
            .await?;
        crate::wire::decode_get(&reply)
    }

    /// Set a record's value (RPC; awaits the server's reply).
    ///
    /// Returns the server's reply: `{status, value, ts}` echoing the latest
    /// write, or `{status}` when the record has no readable latest value. A
    /// reply that echoes a `value` without an integer `ts` is a
    /// [`ClientError::Protocol`].
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
        crate::wire::check_set(serde_json::from_slice(&reply)?)
    }

    /// Subscribe to a record or topic pattern.
    ///
    /// Yields the full [`RecordUpdate`] — value, per-record topic,
    /// [`skipped`](RecordUpdate::skipped) gap count and
    /// [`snapshot_end`](RecordUpdate::snapshot_end) burst marker — so a consumer
    /// that cares about completeness (a watcher, a mirror, an archiver) can see
    /// what the engine dropped. One that only wants values drops the rest:
    ///
    /// ```ignore
    /// let values = conn.subscribe("temp.vienna")?
    ///     .filter_map(|r| futures::future::ready(r.ok().and_then(|u| u.value)));
    /// ```
    ///
    /// Wildcards (`#`, `*`) are supported: one subscription fans in every
    /// matching record, each update tagged with the record that fired. The
    /// engine routes events back by the request id it owns, so there is no
    /// `subscription_id` to track.
    ///
    /// A refused subscription — denied, or past the server's per-connection
    /// limit — yields one `Err` item and then ends, so it stays distinguishable
    /// from a clean end (disconnect, closed record), which is simply `None`.
    /// Dropping the stream stops local delivery.
    pub fn subscribe(
        &self,
        pattern: &str,
    ) -> ClientResult<BoxStream<'static, Result<RecordUpdate, ClientError>>> {
        let raw = self.handle.subscribe(pattern).map_err(rpc_err)?;
        Ok(decode_updates(raw))
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
        drain_response(&reply)
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
        drain_response(&reply)
    }

    /// Run a persistence query (requires the server's `with_persistence()`).
    pub async fn query(&self, params: serde_json::Value) -> ClientResult<serde_json::Value> {
        let reply = self.call("record.query", to_payload(&params)?).await?;
        Ok(serde_json::from_slice(&reply)?)
    }

    /// Which build and which instance this is (`instance.info`, since protocol
    /// 3.1). `None` from a server that does not know the method.
    pub async fn instance_info(&self) -> ClientResult<Option<InstanceInfoReply>> {
        match self.handle.call("instance.info", null_payload()).await {
            Ok(reply) => Ok(Some(from_payload(&reply)?)),
            Err(RpcError::NotFound) => Ok(None),
            Err(e) => Err(rpc_err(e)),
        }
    }

    /// The written post-link bytes (`instance.post_link`, protocol 3.1), read
    /// in chunks of at most [`POST_LINK_CHUNK_MAX`] bytes. `None` when the
    /// server embeds no build info, its region is not written, or it speaks
    /// 3.0.
    ///
    /// Each chunk must start at the requested offset, carry the same `total`
    /// as the first, stay within the bytes requested and within `total`; a
    /// `total` a build-info section cannot hold is refused before reading
    /// further. Any of these from a malformed peer is an error, never a
    /// truncated or padded result.
    pub async fn post_link(&self) -> ClientResult<Option<Vec<u8>>> {
        let mut data = Vec::new();
        let mut total = None;
        loop {
            let offset = data.len();
            let params = json!({ "offset": offset, "len": POST_LINK_CHUNK_MAX });
            let chunk: PostLinkChunk = match self
                .handle
                .call("instance.post_link", to_payload(&params)?)
                .await
            {
                Ok(reply) => from_payload(&reply)?,
                Err(RpcError::NotFound) => return Ok(None),
                Err(e) => return Err(rpc_err(e)),
            };
            let total = *total.get_or_insert(chunk.total);
            let bytes = check_chunk(&chunk, offset, total)?;
            data.extend_from_slice(&bytes);
            if data.len() == total as usize {
                return Ok(Some(data));
            }
        }
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
/// One inbound frame in, one stream item out: an undecodable payload rides
/// through as `value: None`, keeping `skipped` and `snapshot_end` attached to
/// the frame they describe. A snapshot burst can close without any event
/// following it, so metadata held back for a later update may never be
/// delivered at all.
///
/// A terminal rejection yields one `Err` item and then ends the stream. The
/// engine drops the sink after sending one, so the raw stream ends there
/// anyway; the latch holds the contract regardless.
fn decode_updates(
    raw: BoxStream<'static, Result<aimdb_core::session::SubUpdate, RpcError>>,
) -> BoxStream<'static, Result<RecordUpdate, ClientError>> {
    let decoded = raw.scan(false, |terminated, item| {
        if *terminated {
            return futures::future::ready(None);
        }
        let out = match item {
            Ok(u) => Ok(RecordUpdate {
                topic: u.topic.as_deref().map(String::from),
                // A decode failure means a malformed peer, since the server
                // serializes record values as JSON — reported per item rather
                // than fatal, so one bad record cannot end a wildcard stream.
                value: serde_json::from_slice(&u.data).ok(),
                ts: u.ts,
                skipped: u.skipped,
                snapshot_end: u.snapshot_end,
            }),
            Err(e) => {
                *terminated = true;
                Err(sub_err(e))
            }
        };
        futures::future::ready(Some(out))
    });
    Box::pin(decoded)
}

/// Decodes a `record.drain` reply into its public shape.
fn drain_response(reply: &[u8]) -> ClientResult<DrainResponse> {
    let wire = crate::wire::decode_drain(reply)?;
    let (record_name, count) = (wire.record_name.clone(), wire.count);
    Ok(DrainResponse {
        record_name,
        values: wire.values(),
        count,
    })
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

/// The bytes of a post-link `chunk` read at `offset`, if it fits a download
/// whose first chunk said `total`.
fn check_chunk(chunk: &PostLinkChunk, offset: usize, total: u32) -> ClientResult<Vec<u8>> {
    if total as usize >= aimdb_build_info::SECTION_MAX {
        return Err(malformed_chunk(format!(
            "a total of {total} bytes, more than a build-info section holds"
        )));
    }
    if chunk.total != total {
        return Err(malformed_chunk(format!(
            "the total changed from {total} to {} bytes",
            chunk.total
        )));
    }
    if chunk.offset as usize != offset {
        return Err(malformed_chunk(format!(
            "a chunk at offset {} where {offset} was asked for",
            chunk.offset
        )));
    }
    // Checked on the hex, before decoding it.
    let max = POST_LINK_CHUNK_MAX as usize;
    if chunk.data.len() > 2 * max {
        return Err(malformed_chunk(format!(
            "a chunk of {} hex digits, more than the {max} bytes asked for",
            chunk.data.len()
        )));
    }
    let bytes = decode_hex(&chunk.data)?;
    let remaining = total as usize - offset;
    if bytes.len() > remaining {
        return Err(malformed_chunk(format!(
            "{} bytes at offset {offset}, past the total of {total}",
            bytes.len()
        )));
    }
    if bytes.is_empty() && remaining > 0 {
        return Err(malformed_chunk("an empty chunk before the end"));
    }
    Ok(bytes)
}

/// Lowercase or uppercase hex to bytes.
fn decode_hex(hex: &str) -> ClientResult<Vec<u8>> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(malformed_chunk("odd-length hex"));
    }
    pairs
        .iter()
        .map(|&[hi, lo]| Some(nibble(hi)? << 4 | nibble(lo)?))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| malformed_chunk("data is not hex"))
}

fn malformed_chunk(why: impl core::fmt::Display) -> ClientError {
    ClientError::Other(anyhow::anyhow!("malformed instance.post_link reply: {why}"))
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

/// Map the terminal error of a subscription stream onto a [`ClientError`].
///
/// Unlike a call's error it always comes from the server: a dropped
/// connection ends the stream without one. So `Internal` here is not a
/// transport failure but the server ending the subscription, because its
/// source closed or could not be opened (a record buffer with no subscriber
/// slot left, for example).
fn sub_err(e: RpcError) -> ClientError {
    match e {
        RpcError::NotFound | RpcError::Denied => rpc_err(e),
        _ => ClientError::server_error(
            "internal",
            "the server ended the subscription (its source closed or could not be opened)",
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::session::SubUpdate;
    use serde_json::json;

    fn chunk(offset: u32, total: u32, data: &str) -> PostLinkChunk {
        PostLinkChunk {
            offset,
            total,
            data: data.to_string(),
        }
    }

    #[test]
    fn check_chunk_accepts_consistent_chunks() {
        assert_eq!(
            check_chunk(&chunk(0, 3, "aabbcc"), 0, 3).unwrap(),
            vec![0xaa, 0xbb, 0xcc]
        );
        assert_eq!(
            check_chunk(&chunk(1024, 1500, &"00".repeat(476)), 1024, 1500).unwrap(),
            vec![0; 476]
        );
        // Nothing written past offset 0: an empty region.
        assert!(check_chunk(&chunk(0, 0, ""), 0, 0).unwrap().is_empty());
    }

    #[test]
    fn check_chunk_rejects_inconsistent_chunks() {
        let max = aimdb_build_info::SECTION_MAX as u32;
        let full = "00".repeat(POST_LINK_CHUNK_MAX as usize);
        for (case, chunk, offset, total) in [
            ("total a section cannot hold", chunk(0, max, ""), 0, max),
            ("huge total", chunk(0, u32::MAX, "00"), 0, u32::MAX),
            ("total changed", chunk(1024, 3000, &full), 1024, 2048),
            (
                "offset not the one asked for",
                chunk(0, 2048, &full),
                1024,
                2048,
            ),
            (
                "more than asked for",
                chunk(0, 4096, &format!("{full}00")),
                0,
                4096,
            ),
            ("past the total", chunk(0, 2, "aabbcc"), 0, 2),
            (
                "past the total, later chunk",
                chunk(1024, 1500, &full),
                1024,
                1500,
            ),
            ("empty before the end", chunk(4, 8, ""), 4, 8),
            ("not hex", chunk(0, 1, "zz"), 0, 1),
        ] {
            let err = check_chunk(&chunk, offset, total).expect_err(case);
            assert!(
                err.to_string()
                    .contains("malformed instance.post_link reply"),
                "{case}: {err}"
            );
        }
    }

    #[test]
    fn decode_hex_accepts_both_cases_and_rejects_the_rest() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00ff7A").unwrap(), vec![0x00, 0xff, 0x7a]);
        assert!(decode_hex("abc").is_err(), "odd length");
        assert!(decode_hex("zz").is_err(), "not hex");
        // Multi-byte UTF-8 must not panic on a char boundary.
        assert!(decode_hex("é0").is_err());
        assert!(decode_hex("0é").is_err());
    }

    /// Build a raw engine stream from a fixed list of subscription items.
    fn raw_stream(
        items: Vec<Result<SubUpdate, RpcError>>,
    ) -> BoxStream<'static, Result<SubUpdate, RpcError>> {
        Box::pin(futures::stream::iter(items))
    }

    fn ok_update(payload: &str, skipped: u64) -> Result<SubUpdate, RpcError> {
        // Stamped with the payload's length, so each test value has its own.
        let ts = Stamp::from_nanos(payload.len() as u64);
        Ok(SubUpdate::new(Payload::from(payload.as_bytes()), ts).with_skipped(skipped))
    }

    /// Collect a decoded stream, unwrapping every item as a delivered update.
    async fn decoded(items: Vec<Result<SubUpdate, RpcError>>) -> Vec<RecordUpdate> {
        decode_updates(raw_stream(items))
            .map(|r| r.expect("no terminal error in this stream"))
            .collect()
            .await
    }

    #[tokio::test]
    async fn gap_counts_reach_the_caller() {
        let out = decoded(vec![ok_update(r#"{"n":1}"#, 0), ok_update(r#"{"n":2}"#, 7)]).await;

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].value, Some(json!({ "n": 1 })));
        assert_eq!(
            out[0].ts,
            Stamp::from_nanos(7),
            "the frame's stamp reaches the caller"
        );
        assert_eq!(out[0].skipped, 0);
        assert!(!out[0].has_gap());
        assert_eq!(out[1].value, Some(json!({ "n": 2 })));
        assert_eq!(out[1].skipped, 7, "the engine's gap count is preserved");
        assert!(out[1].has_gap());
    }

    /// The burst's final update surfaces `snapshot_end` together with the loss
    /// the burst accumulated.
    #[tokio::test]
    async fn snapshot_end_and_its_loss_reach_the_caller() {
        let out = decoded(vec![
            ok_update(r#"{"n":1}"#, 0),
            Ok(
                SubUpdate::new(Payload::from(&b"{\"n\":2}"[..]), Stamp::from_nanos(2))
                    .with_skipped(41)
                    .with_snapshot_end(),
            ),
            ok_update(r#"{"n":3}"#, 0), // a live event, past the burst
        ])
        .await;

        assert_eq!(out.len(), 3);
        assert!(!out[0].snapshot_end);
        assert!(out[1].snapshot_end, "the burst's final update is flagged");
        assert_eq!(out[1].skipped, 41, "carrying the burst's whole loss");
        assert!(!out[2].snapshot_end, "a live event does not close a burst");
    }

    /// A burst-final frame that fails to decode still closes the burst: it
    /// arrives as `value: None` carrying its own `snapshot_end`. Nothing
    /// follows it here, which is why the marker cannot ride a later update.
    #[tokio::test]
    async fn an_undecodable_burst_final_frame_still_closes_the_burst() {
        let out = decoded(vec![
            ok_update(r#"{"n":1}"#, 0),
            Ok(
                SubUpdate::new(Payload::from(&b"not json"[..]), Stamp::from_nanos(3))
                    .with_skipped(3)
                    .with_snapshot_end(),
            ),
        ])
        .await;

        assert_eq!(out.len(), 2, "an undecodable frame is still delivered");
        assert_eq!(out[1].value, None, "its payload did not decode");
        assert!(
            out[1].snapshot_end,
            "burst completion must not depend on a later update that may never arrive"
        );
        assert_eq!(out[1].skipped, 3, "and its gap count stays exact");
    }

    #[tokio::test]
    async fn topics_ride_along_and_rejection_ends_the_stream() {
        let raw = raw_stream(vec![
            Ok(SubUpdate::tagged(
                Arc::from("temp.vienna"),
                Payload::from(&b"1"[..]),
                Stamp::from_nanos(11),
            )
            .with_skipped(2)),
            Err(RpcError::Denied),
            ok_update("99", 0), // past the terminal item — never delivered
        ]);
        let out: Vec<_> = decode_updates(raw).collect().await;

        assert_eq!(out.len(), 2, "the rejection is the stream's last item");
        let first = out[0].as_ref().expect("delivered update");
        assert_eq!(first.topic.as_deref(), Some("temp.vienna"));
        assert_eq!(first.value, Some(json!(1)));
        assert_eq!(first.ts, Stamp::from_nanos(11));
        assert_eq!(first.skipped, 2);

        // The refusal is an `Err` item rather than a silent EOF, so a caller can
        // tell it from a disconnect and fail instead of reporting a clean stop.
        let err = out[1]
            .as_ref()
            .expect_err("the rejection reaches the caller");
        assert!(
            matches!(err, ClientError::ServerError { code, .. } if code == "denied"),
            "unexpected terminal error: {err}"
        );
    }

    /// A subscription the server ended (`Internal` as its terminal item) says
    /// so, rather than blaming the transport: a dropped connection would have
    /// ended the stream without an error.
    #[tokio::test]
    async fn a_subscription_the_server_ended_says_so() {
        let raw = raw_stream(vec![ok_update("1", 0), Err(RpcError::Internal)]);
        let out: Vec<_> = decode_updates(raw).collect().await;

        assert_eq!(out.len(), 2);
        let err = out[1].as_ref().expect_err("the end reaches the caller");
        assert!(
            matches!(
                err,
                ClientError::ServerError { code, message, .. }
                    if code == "internal" && message.contains("ended the subscription")
            ),
            "unexpected terminal error: {err}"
        );
    }

    /// A call's `Internal` keeps naming the engine or transport: for calls the
    /// client engine raises it itself when the connection drops.
    #[test]
    fn a_calls_internal_error_still_names_the_transport() {
        let err = rpc_err(RpcError::Internal);
        assert!(
            matches!(
                &err,
                ClientError::ServerError { message, .. } if message == "engine/transport failure"
            ),
            "{err}"
        );
        assert!(matches!(
            sub_err(RpcError::Denied),
            ClientError::ServerError { code, .. } if code == "denied"
        ));
    }
}
