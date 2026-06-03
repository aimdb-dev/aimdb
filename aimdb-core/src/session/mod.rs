//! Connector-session substrate — the shared machinery for transport-based
//! remote access.
//!
//! Three layers: transport ([`Connection`]/[`Listener`]/[`Dialer`]), codec
//! ([`EnvelopeCodec`]), and dispatch ([`Dispatch`]/[`Session`]), over a
//! role-neutral [`Inbound`]/[`Outbound`] message set shared by the reactive
//! server engine (`serve`/`run_session`) and the proactive client engine
//! (`run_client`/`pump_client`). Data-plane connectors use `pump_sink`/
//! `pump_source` over the [`Source`] / [`Connector`](crate::transport::Connector)
//! capabilities.
//!
//! All contracts are `dyn`-safe and compile on `std` and `no_std + alloc`. See
//! `docs/design/remote-access-via-connectors.md` for the design.

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::future::Future;
use core::pin::Pin;

use futures_core::Stream;

// The engines are runtime-neutral (`futures` channels + the adapter's `TimeOps`
// clock, no `tokio`/`embassy-*`), so they cross-compile to `no_std + alloc`.
#[cfg(feature = "connector-session")]
mod client;
#[cfg(feature = "connector-session")]
mod connector;
#[cfg(feature = "connector-session")]
mod pump;
#[cfg(feature = "connector-session")]
mod server;

// Concrete AimX protocol substrate. The codec is `no_std + alloc`; the server
// dispatch is `std`-gated. The transport lives in a separate connector crate
// (`aimdb-uds-connector`) — core keeps the protocol plus the generic
// [`SessionClientConnector`] / [`SessionServerConnector`] spine.
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

/// Boxed, `Send` future — the object-safe async return shape used throughout.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed, `Send` stream — the reply shape of a subscription ([`Session::subscribe`]).
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// A serialized record value, carried opaquely through the codec.
///
/// `Arc<[u8]>` so fan-out is a cheap refcount bump; bytes stay opaque on the hot
/// path, with structured (`serde_json::Value`) conversion only where a handler
/// inspects them.
pub type Payload = Arc<[u8]>;

/// Result of a transport-layer operation.
pub type TransportResult<T> = Result<T, TransportError>;

// ===========================================================================
// Supporting types (stubs — sufficient for the signatures to compile)
// ===========================================================================

/// Remote-peer metadata carried by a [`Connection`].
///
/// A neutral [`peer_addr`](Self::peer_addr) plus a type-erased
/// [`ext`](Self::ext) slot a connector fills with its own resolved identity
/// (e.g. WS attaches `ClientInfo` at the HTTP upgrade), keeping core
/// connector-agnostic. Downcast `ext` with [`ext_as`](Self::ext_as).
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

/// The authenticated session context produced by [`Dispatch::authenticate`] and
/// threaded into [`Dispatch::open`].
///
/// Carries the resolved principal as a type-erased [`ext`](Self::ext) for
/// per-operation authorization in the [`Session`]; downcast with
/// [`ext_as`](Self::ext_as). Connectors that don't authenticate leave it `None`.
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

/// Per-session resource bounds consumed by the engines.
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
// Logical message set — role-neutral: the server's `Inbound` is the client's
// outbound and vice versa.
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
    /// [`run_session`] only when [`SessionConfig::acks_subscribe`] is set.
    /// The `sub` is the subscription's routing id — the same value that tags its
    /// [`Event`](Outbound::Event)s.
    Subscribed {
        /// Subscription id that was opened.
        sub: &'a str,
    },
    /// Keepalive response.
    Pong,
}

// ===========================================================================
// Layer 1 — transport. Framing lives in the transport: `recv` returns one
// logical frame. `Dialer` is the client-side dual of `Listener`.
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
// Layer 3 — dispatch. RPC and streaming unify in one per-connection role with
// three reply cardinalities: `call` (one) / `subscribe` (many) / `write` (none).
// Split across two traits: the shared, immutable [`Dispatch`] (one
// `Arc<dyn Dispatch>` per server) and the per-connection, `&mut`-threaded
// [`Session`] that can hold mutable state without a lock.
// ===========================================================================

/// The shared application dispatch: authenticate a connection, then open a
/// per-connection [`Session`]. One `Arc<dyn Dispatch>` is shared across every
/// accepted connection, so it stays `Send + Sync` and behind `&self`.
pub trait Dispatch: Send + Sync {
    /// Resolve a [`SessionCtx`] from peer metadata and/or the first frame
    /// (a pre-resolved identity in [`PeerInfo`], or an in-band Hello in `first`).
    fn authenticate<'a>(
        &'a self,
        peer: &'a PeerInfo,
        first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>>;

    /// Open the per-connection [`Session`] once, after [`authenticate`](Self::authenticate). It owns
    /// the connection's mutable dispatch state that the shared `Arc<Self>` cannot
    /// hold behind `&self`; the engine threads `&mut` into it.
    fn open(&self, ctx: &SessionCtx) -> Box<dyn Session>;
}

/// The per-connection session: serves calls, subscriptions, and writes for one
/// accepted [`Connection`]. The engine owns the `Box<dyn Session>` and threads
/// `&mut self` into each method, so it can hold per-connection state without a
/// lock; the shared, immutable role stays on [`Dispatch`].
pub trait Session: Send {
    /// One-shot RPC: one request → one reply.
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>>;

    /// Open a subscription yielding many payloads. The stream is `'static` (it
    /// captures cloned handles) so it outlives the `&mut` borrow and lives in the
    /// engine. Async so a connector can await per-operation authorization.
    /// Defaulted to [`RpcError::NotFound`] for dispatches with no streaming.
    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, Payload>, RpcError>> {
        let _ = topic;
        Box::pin(async { Err(RpcError::NotFound) })
    }

    /// Late-join snapshot: the current value for `topic`, emitted as an
    /// [`Outbound::Snapshot`] right after a successful
    /// [`subscribe`](Session::subscribe) and before the first event. Defaulted to
    /// `None` (no snapshot).
    fn snapshot(&mut self, topic: &str) -> Option<Payload> {
        let _ = topic;
        None
    }

    /// Fire-and-forget write: no reply. Routes through the producer/arbiter path,
    /// so single-writer-per-key stays intact.
    fn write<'a>(
        &'a mut self,
        topic: &'a str,
        payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>>;
}

/// The protocol-envelope codec: frame bytes ↔ the logical message set. Layered
/// above (and nesting) the record-value `JsonCodec`, so the wire format stays
/// pluggable; `decode`/`encode` keep the record value as an opaque [`Payload`]
/// sliced from / spliced into the frame.
///
/// Symmetric, so one codec object serves both engines: `decode`/`encode` are the
/// server direction (read requests / write replies) and
/// [`encode_inbound`](EnvelopeCodec::encode_inbound) /
/// [`decode_outbound`](EnvelopeCodec::decode_outbound) the client direction.
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
// Data-plane capabilities — connectionless (an external library owns any
// session). The outbound `Sink` is the canonical
// [`Connector`](crate::transport::Connector); the inbound `Source` is below.
// ===========================================================================

/// External → AimDB data-plane: a stream of inbound frames, drained by
/// `pump_source`.
pub trait Source: Send {
    /// Yield the next `(topic, payload)`, or `None` when the source is done.
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>>;
}

// ===========================================================================
// Object-safety: taking each trait as `&dyn Trait` forces the dyn-compatibility
// check on all targets, not just under `cargo test`.
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

    /// Every trait is `dyn`-usable.
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
