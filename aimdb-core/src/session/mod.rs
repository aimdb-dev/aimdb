//! Frozen Phase 0 connector-session contracts (trait skeletons only).
//!
//! This module locks the cross-cutting trait **signatures** that every later
//! phase of the connector-convergence initiative (issue #39 — embedded remote
//! access) depends on. It ships **contracts, not behavior**: every method body
//! is `unimplemented!()`. The engines (`run_session` / `serve` / `run_client`),
//! the pump helpers, and the transport/dispatch impls all arrive in Phases 1–6.
//!
//! See [`docs/design/remote-access-via-connectors.md`] for the design — the
//! decisions, the three-layer substrate (transport + [`EnvelopeCodec`] +
//! [`Dispatch`]), and the capability model. `Sink` is now the canonical
//! [`Connector`](crate::transport::Connector); [`Source`] / [`Dialer`] are here.
//!
//! Everything here is `dyn`-safe and compiles on `std` **and** `no_std + alloc`
//! (boxed-future pattern throughout, no `std`/`tokio`/`serde_json` at the
//! contract level).

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::future::Future;
use core::pin::Pin;

use futures_core::Stream;

// ---------------------------------------------------------------------------
// Phase 2 engines. **Phase 5 made these runtime-neutral** (`futures` channels +
// `select_biased!` + the adapter's `TimeOps` clock — no `tokio`/`embassy-*`), so
// they now gate on `connector-session` (`alloc`) rather than `std` and
// cross-compile to `thumbv7em-none-eabihf`. The frozen contracts above stay
// `no_std + alloc` as before. Only the concrete AimX substrate below (UDS +
// NDJSON) is still std-only until its embedded transport lands in Phase 6.
// See docs/design/remote-access-via-connectors.md (Phases 0/2/5).
// ---------------------------------------------------------------------------

#[cfg(feature = "connector-session")]
mod client;
#[cfg(feature = "connector-session")]
mod connector;
#[cfg(feature = "connector-session")]
mod pump;
#[cfg(feature = "connector-session")]
mod server;

// Concrete AimX-v2 protocol substrate (NDJSON codec + server dispatch). The
// codec is `no_std + alloc` (the embedded *client* rides it over a real
// transport); the dispatch stays `std`-gated inside the module until its own
// no_std port. The transport itself is no longer here — it is a swappable
// connector crate (`aimdb-uds-connector`); core keeps the protocol + the
// generic [`SessionClientConnector`] / [`SessionServerConnector`] spine.
#[cfg(all(feature = "connector-session", feature = "json-serialize"))]
pub mod aimx;

#[cfg(feature = "connector-session")]
pub use client::{pump_client, run_client, ClientConfig, ClientHandle};
#[cfg(feature = "connector-session")]
pub use connector::{SessionClientConnector, SessionServerConnector};
#[cfg(feature = "connector-session")]
pub use pump::{pump_sink, pump_source};
#[cfg(feature = "connector-session")]
pub use server::{run_session, serve, SessionConfig};

// ===========================================================================
// Shared aliases
// ===========================================================================

/// Boxed, `Send` future — the object-safe async return shape used by every
/// trait here, matching the existing `Connector` / `ProducerTrait` pattern.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed, `Send` stream — the reply shape of a subscription
/// ([`Dispatch::subscribe`]).
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// The record-value seam between the outer [`EnvelopeCodec`] and the inner M16
/// record-value `JsonCodec` (Decision 1: **raw bytes**).
///
/// Opaque serialized bytes; cheap-clone (refcount bump) for WS fan-out,
/// `no_std + alloc`-native, no new dependency. Bytes flow opaque through the hot
/// paths; typed/structured conversion happens only at the ends that need it
/// (`serde_json::Value` materializes only inside RPC handlers that inspect
/// structure). `bytes::Bytes` is reserved for a later need (cheap sub-slicing /
/// zero-copy binary framing).
pub type Payload = Arc<[u8]>;

/// Result of a transport-layer operation.
pub type TransportResult<T> = Result<T, TransportError>;

// ===========================================================================
// Supporting types (stubs — sufficient for the signatures to compile)
// ===========================================================================

/// Remote-peer metadata carried by a [`Connection`] (remote addr, pre-resolved
/// auth).
///
/// **Phase 4 (resolved — doc 037 auth-context gate).** One shape serves both
/// connectors: a neutral [`peer_addr`](Self::peer_addr) plus a type-erased
/// [`ext`](Self::ext) slot the connector fills with its own resolved identity
/// (WS stuffs `ClientInfo`/`Permissions` at the HTTP upgrade; AimX stuffs its
/// `SecurityPolicy`). Core stays connector-agnostic; each side downcasts `ext`.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct PeerInfo {
    /// Remote address, if the transport exposes one.
    pub peer_addr: Option<String>,
    /// Connector-resolved identity, type-erased so core need not know the
    /// connector's auth types. Downcast with [`ext_as`](Self::ext_as).
    pub ext: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

impl PeerInfo {
    /// Attach a connector-resolved identity (consumed by [`Dispatch::authenticate`]).
    pub fn with_ext(mut self, ext: Arc<dyn core::any::Any + Send + Sync>) -> Self {
        self.ext = Some(ext);
        self
    }

    /// Downcast the [`ext`](Self::ext) identity to a concrete connector type.
    pub fn ext_as<T: core::any::Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.ext.clone()?.downcast::<T>().ok()
    }
}

impl core::fmt::Debug for PeerInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PeerInfo")
            .field("peer_addr", &self.peer_addr)
            .field("ext", &self.ext.as_ref().map(|_| "<opaque>"))
            .finish()
    }
}

/// The authenticated session context threaded through [`Dispatch`] calls.
///
/// **Phase 4 (resolved — doc 037 auth-context gate).** Carries the resolved
/// principal as a type-erased [`ext`](Self::ext) that [`Dispatch::open`] threads
/// into the per-connection [`Session`] for per-operation authorization
/// (`authorize_subscribe`/`authorize_write`). AimX leaves it `None`.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct SessionCtx {
    /// The resolved principal, type-erased. Downcast with [`ext_as`](Self::ext_as).
    pub ext: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

impl SessionCtx {
    /// Build a context carrying a connector-resolved principal.
    pub fn with_ext(ext: Arc<dyn core::any::Any + Send + Sync>) -> Self {
        Self { ext: Some(ext) }
    }

    /// Downcast the [`ext`](Self::ext) principal to a concrete connector type.
    pub fn ext_as<T: core::any::Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.ext.clone()?.downcast::<T>().ok()
    }
}

impl core::fmt::Debug for SessionCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionCtx")
            .field("ext", &self.ext.as_ref().map(|_| "<opaque>"))
            .finish()
    }
}

/// Engine-local bounds for a session (consumed by the Phase 2 engines, not by
/// the contracts here).
///
/// Whether these become `heapless`/const-generic vs runtime config is
/// **deferred to Phase 5** (bounded-resource policy).
#[derive(Debug, Clone)]
pub struct SessionLimits {
    /// Maximum concurrently served connections.
    pub max_connections: usize,
    /// Maximum live subscriptions per connection.
    pub max_subs_per_connection: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_connections: 16,
            max_subs_per_connection: 32,
        }
    }
}

/// Transport-layer failure (Layer 1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportError {
    /// The connection was closed or reset by the peer.
    Closed,
    /// An underlying I/O operation failed.
    Io,
}

/// Envelope-codec failure — a frame could not be decoded/encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodecError {
    /// The frame was not valid for this envelope format.
    Malformed,
}

/// Dispatch-layer (application) failure for `call` / `subscribe` / `write`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RpcError {
    /// No such method or topic.
    NotFound,
    /// The caller lacks permission for this operation.
    Denied,
    /// The handler failed.
    Internal,
}

/// Authentication failure raised by [`Dispatch::authenticate`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
    /// Credentials were missing or rejected.
    Unauthorized,
}

// ===========================================================================
// Logical message set (role-neutral; the server's `Inbound` is the client's
// out-bound and vice versa — doc 034 § The substrate is shared with a client
// engine). Field types align with the existing AimX wire (`remote::protocol`).
// ===========================================================================

/// A logical request arriving over a [`Connection`] (what the server receives).
pub enum Inbound {
    /// An RPC call expecting a single [`Outbound::Reply`].
    Request {
        /// Correlation id, echoed in the reply.
        id: u64,
        /// Method name (e.g. `"record.set"`, `"query"`).
        method: String,
        /// Unparsed method parameters.
        params: Payload,
    },
    /// Open a subscription producing many [`Outbound::Event`]s.
    Subscribe {
        /// Correlation id for the subscription handshake.
        id: u64,
        /// Topic to subscribe to.
        topic: String,
    },
    /// Close a previously opened subscription.
    Unsubscribe {
        /// Subscription id to cancel.
        sub: String,
    },
    /// A fire-and-forget write (no reply).
    Write {
        /// Destination topic.
        topic: String,
        /// Unparsed record value.
        payload: Payload,
    },
    /// Keepalive.
    Ping,
}

/// A logical message sent back over a [`Connection`] (what the server emits).
pub enum Outbound<'a> {
    /// Reply to an [`Inbound::Request`].
    Reply {
        /// Correlation id of the originating request.
        id: u64,
        /// The result, or an [`RpcError`].
        result: Result<Payload, RpcError>,
    },
    /// A subscription update.
    Event {
        /// Subscription id this event belongs to.
        sub: &'a str,
        /// Monotonic sequence number.
        seq: u64,
        /// Unparsed record value.
        data: Payload,
    },
    /// An initial snapshot emitted when a subscription opens (late-join).
    Snapshot {
        /// Topic the snapshot is for.
        topic: &'a str,
        /// Unparsed record value.
        data: Payload,
    },
    /// An explicit acknowledgement that a subscription opened. Emitted by
    /// [`run_session`](super::server::run_session) only when
    /// [`SessionConfig::acks_subscribe`](super::server::SessionConfig) is set
    /// (WS needs it; AimX's ack is implicit, so it leaves the flag off and never
    /// emits this). The `sub` is the subscription's routing id — the same value
    /// that tags its [`Event`](Outbound::Event)s.
    Subscribed {
        /// Subscription id that was opened.
        sub: &'a str,
    },
    /// Keepalive response.
    Pong,
}

// ===========================================================================
// Layer 1 — transport (the std / Embassy seam). Doc 034 § Layer 1; `Dialer`
// from doc 035 § The toolkit (the dual of `Listener`). Framing lives *in* the
// transport: `recv` returns one logical frame.
// ===========================================================================

/// A framed, bidirectional pipe — role-neutral (yielded by either
/// [`Listener::accept`] or [`Dialer::connect`]).
pub trait Connection: Send {
    /// Receive one logical frame. `Ok(None)` signals the peer closed.
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>>;

    /// Send one logical frame.
    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>>;

    /// Peer metadata (remote addr, headers, pre-resolved auth).
    fn peer(&self) -> &PeerInfo;
}

/// The accepting (server) side — produces [`Connection`]s we did not initiate.
pub trait Listener: Send {
    /// Accept the next inbound connection.
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>>;
}

/// The initiating (client) side — the dual of [`Listener`]; dials out and
/// produces the same [`Connection`].
pub trait Dialer: Send {
    /// Open a connection to the configured remote.
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>>;
}

// ===========================================================================
// Layer 3 — dispatch (the semantics). Doc 034 § Layer 3 + § EnvelopeCodec.
// RPC and streaming unify in ONE per-connection role (Decision 2): three reply
// cardinalities — `call` (one) / `subscribe` (many) / `write` (none).
//
// The role is split across two traits so the shared, immutable half (one
// `Arc<dyn Dispatch>` per server) and the per-connection mutable half (one
// `Box<dyn Session>` per accepted connection) each own what they need:
//
// - [`Dispatch`] — `Send + Sync`, shared: `authenticate` + an `open` factory.
// - [`Session`]  — `Send`, per-connection: `call` / `subscribe` / `write` on
//   `&mut self`, so a connection can hold mutable state (e.g. `record.drain`'s
//   lazy per-record cursors — the one seam the AimX wire reshape did not
//   dissolve) without a lock. See doc 037 (the additive server-port refinement,
//   mirroring the Phase-2 `encode_inbound`/`decode_outbound` precedent).
// ===========================================================================

/// The shared application dispatch: authenticate a connection, then open a
/// per-connection [`Session`]. One `Arc<dyn Dispatch>` is shared across every
/// connection a server accepts, so it stays `Sync` and behind `&self`.
pub trait Dispatch: Send + Sync {
    /// Resolve a [`SessionCtx`] from peer metadata and/or the first frame
    /// (WS supplies pre-resolved identity via [`PeerInfo`]; UDS reads a Hello).
    fn authenticate<'a>(
        &'a self,
        peer: &'a PeerInfo,
        first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>>;

    /// Open the per-connection [`Session`] once, after [`authenticate`]. The
    /// returned session owns the connection's mutable dispatch state (drain
    /// cursors today, per-session auth identity in Phase 4) that the shared
    /// `Arc<Self>` cannot hold behind `&self`; the engine threads `&mut` into it.
    fn open(&self, ctx: &SessionCtx) -> Box<dyn Session>;
}

/// The per-connection session: serves calls, subscriptions, and writes for one
/// accepted [`Connection`]. The engine ([`run_session`]) owns the
/// `Box<dyn Session>` and threads `&mut self` into each method, so a session can
/// hold per-connection mutable state without a lock — while the shared,
/// immutable role stays on [`Dispatch`].
pub trait Session: Send {
    /// One-shot RPC: one request → one reply.
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>>;

    /// Streaming: open a subscription that yields many payloads. The stream is
    /// `'static` (it captures cloned handles), so it outlives the `&mut` borrow
    /// and lives in the engine's `FuturesUnordered` (doc 034 risk list).
    ///
    /// Defaulted to [`RpcError::NotFound`] so a dispatch with no streaming
    /// surface need not implement it (doc 037 § the server-port refinement —
    /// the stream is side-neutral, so it is defaulted here for symmetry).
    ///
    /// Async (Phase 4): opening a subscription may need to *await* per-operation
    /// authorization (e.g. WS `authorize_subscribe`); the engine awaits it.
    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
        let _ = topic;
        Box::pin(async { Err(RpcError::NotFound) })
    }

    /// Late-join snapshot: the current value for `topic`, emitted by
    /// [`run_session`](super::server::run_session) as an [`Outbound::Snapshot`]
    /// right after a successful [`subscribe`](Session::subscribe) and before the
    /// first event. Defaulted to `None` (no snapshot) — AimX inherits this; WS
    /// overrides it from its `SnapshotProvider`.
    fn snapshot(&mut self, topic: &str) -> Option<Payload> {
        let _ = topic;
        None
    }

    /// Fire-and-forget write: no reply. Routes through the existing
    /// producer/arbiter path (single-writer-per-key stays intact).
    fn write<'a>(
        &'a mut self,
        topic: &'a str,
        payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>>;
}

/// The protocol-envelope codec: frame bytes ↔ one logical message set. Distinct
/// from, and layered above, the M16 record-value `JsonCodec` it nests; the wire
/// format (NDJSON / WS-JSON / `serde-json-core`) stays pluggable.
///
/// Per Decision 1, `decode` yields `params`/`data` as an *unparsed* [`Payload`]
/// (a slice of the frame) and `encode` splices a [`Payload`] in verbatim.
///
/// **Symmetric (both engines, one codec).** The first pair below is the
/// *server* direction (read requests / write replies), frozen in Phase 0. The
/// [`encode_inbound`](EnvelopeCodec::encode_inbound) /
/// [`decode_outbound`](EnvelopeCodec::decode_outbound) pair is the *client*
/// direction (write requests / read replies), added in Phase 2 so `run_client`
/// reuses the **same** codec object rather than a per-role copy — the
/// role-neutral-substrate invariant (doc 036). The two frozen signatures are
/// unchanged; this is purely additive.
pub trait EnvelopeCodec: Send + Sync {
    /// Decode one frame into a logical [`Inbound`] message (server reads a request).
    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError>;

    /// Encode a logical [`Outbound`] message, appending its bytes to `out`
    /// (server writes a reply/event).
    fn encode(&self, msg: Outbound<'_>, out: &mut Vec<u8>) -> Result<(), CodecError>;

    /// Encode a logical [`Inbound`] message, appending its bytes to `out`
    /// (client writes a request). The dual of [`decode`](EnvelopeCodec::decode).
    fn encode_inbound(&self, msg: Inbound, out: &mut Vec<u8>) -> Result<(), CodecError>;

    /// Decode one frame into a logical [`Outbound`] message (client reads a
    /// reply/event). The dual of [`encode`](EnvelopeCodec::encode); the result
    /// borrows the frame (`Outbound`'s `sub`/`topic` are `&str` slices into it).
    fn decode_outbound<'a>(&self, frame: &'a [u8]) -> Result<Outbound<'a>, CodecError>;
}

// ===========================================================================
// Data-plane capabilities (doc 035 § The toolkit). Connectionless: an external
// library owns any session.
// ===========================================================================

// AimDB → external data-plane (the `Sink` capability) is the canonical
// [`Connector`](crate::transport::Connector) trait verbatim — Phase 1 collapsed
// the Phase-0 `Sink` skeleton onto it. `pump_sink` takes `Arc<dyn Connector>`.

/// External → AimDB data-plane — a stream of inbound frames (the one genuinely
/// new data-plane trait; replaces the hand-rolled read loop).
pub trait Source: Send {
    /// Yield the next `(topic, payload)`, or `None` when the source is done.
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>>;
}

// ===========================================================================
// Object-safety: referencing each trait as `&dyn Trait` in non-test code forces
// the dyn-compatibility check on *all* targets (std and `no_std + alloc`), not
// just under `cargo test`. The `#[cfg(test)]` block below additionally builds a
// `Box<dyn Trait>` from a mock per the acceptance criteria.
// ===========================================================================

#[allow(dead_code, clippy::too_many_arguments)]
fn _assert_object_safe(
    _connection: &dyn Connection,
    _listener: &dyn Listener,
    _dialer: &dyn Dialer,
    _dispatch: &dyn Dispatch,
    _session: &dyn Session,
    _codec: &dyn EnvelopeCodec,
    _source: &dyn Source,
) {
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockConnection;
    impl Connection for MockConnection {
        fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
            unimplemented!()
        }
        fn send<'a>(&'a mut self, _frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
            unimplemented!()
        }
        fn peer(&self) -> &PeerInfo {
            unimplemented!()
        }
    }

    struct MockListener;
    impl Listener for MockListener {
        fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
            unimplemented!()
        }
    }

    struct MockDialer;
    impl Dialer for MockDialer {
        fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
            unimplemented!()
        }
    }

    struct MockDispatch;
    impl Dispatch for MockDispatch {
        fn authenticate<'a>(
            &'a self,
            _peer: &'a PeerInfo,
            _first: Option<&'a [u8]>,
        ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
            unimplemented!()
        }
        fn open(&self, _ctx: &SessionCtx) -> Box<dyn Session> {
            unimplemented!()
        }
    }

    struct MockSession;
    impl Session for MockSession {
        fn call<'a>(
            &'a mut self,
            _method: &'a str,
            _params: Payload,
        ) -> BoxFut<'a, Result<Payload, RpcError>> {
            unimplemented!()
        }
        fn subscribe<'a>(
            &'a mut self,
            _topic: &'a str,
        ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
            unimplemented!()
        }
        fn write<'a>(
            &'a mut self,
            _topic: &'a str,
            _payload: Payload,
        ) -> BoxFut<'a, Result<(), RpcError>> {
            unimplemented!()
        }
    }

    struct MockCodec;
    impl EnvelopeCodec for MockCodec {
        fn decode(&self, _frame: &[u8]) -> Result<Inbound, CodecError> {
            unimplemented!()
        }
        fn encode(&self, _msg: Outbound<'_>, _out: &mut Vec<u8>) -> Result<(), CodecError> {
            unimplemented!()
        }
        fn encode_inbound(&self, _msg: Inbound, _out: &mut Vec<u8>) -> Result<(), CodecError> {
            unimplemented!()
        }
        fn decode_outbound<'a>(&self, _frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
            unimplemented!()
        }
    }

    struct MockSource;
    impl Source for MockSource {
        fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
            unimplemented!()
        }
    }

    /// Acceptance criterion: every frozen trait is `dyn`-usable.
    #[test]
    fn traits_are_object_safe() {
        let _connection: Box<dyn Connection> = Box::new(MockConnection);
        let _listener: Box<dyn Listener> = Box::new(MockListener);
        let _dialer: Box<dyn Dialer> = Box::new(MockDialer);
        let _dispatch: Box<dyn Dispatch> = Box::new(MockDispatch);
        let _session: Box<dyn Session> = Box::new(MockSession);
        let _codec: Box<dyn EnvelopeCodec> = Box::new(MockCodec);
        let _source: Box<dyn Source> = Box::new(MockSource);
    }
}
