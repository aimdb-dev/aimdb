//! Runtime-neutral byte-stream layer (design 052 §3.1) — the seam that lets a
//! connector own framing and protocol while an *adapter* owns sockets, clocks
//! and name resolution.
//!
//! It sits one layer **below** [`Connection`](super::Connection): a
//! [`ByteStream`] is unframed, a [`Connection`](super::Connection) is framed.
//! [`FramedConnection`] joins the two, and [`FramingDialer`] /
//! [`FramingListener`] lift a [`StreamDialer`] / [`StreamListener`] into the
//! existing [`Dialer`](super::Dialer) / [`Listener`](super::Listener), so
//! `run_client` and `serve` are untouched.
//!
//! # Why the `Send` bound sits on the trait's return type
//!
//! Generic connector code has to produce `Send` futures at the boxing boundary
//! (`ConnectorBuilder::build` returns `Vec<BoxFuture>`; `Connection::recv`
//! returns [`BoxFut`](super::BoxFut)). A generic `S: embedded_io_async::Read`
//! cannot prove `S::read(..)` yields a `Send` future, and return-type notation
//! — which would express exactly that bound — is still experimental on the
//! pinned toolchain (`error[E0658]`). Declaring `+ Send` on the trait's
//! return type gives generic code the bound for free, with nothing boxed:
//!
//! - a `std` impl writes a plain `async fn` and the compiler checks it;
//! - an Embassy impl returns its force-`Send` newtype over the `!Send` inner
//!   future — a transparent wrapper, zero runtime cost;
//! - [`FramedConnection`] boxes once per **frame**, exactly as the Embassy
//!   adapter's `EmbassyConnection` does today. Nothing new is boxed per chunk.
//!
//! The traits are deliberately **not** `dyn`-compatible. Connectors are
//! generic over them; the `dyn` boundary stays where it is today, at
//! `Box<dyn Connection>` per frame.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;

use super::{BoxFut, Connection, Dialer, Listener, PeerInfo, TransportError, TransportResult};

/// An unframed, bidirectional byte stream — one TCP connection, one UART, one
/// TLS session. The adapter owns it; the connector never names its type.
///
/// `read` returning `Ok(0)` means end of stream, matching
/// `embedded_io_async::Read` and `tokio::io::AsyncRead`.
pub trait ByteStream {
    /// Read into `buf`, returning the byte count; `Ok(0)` is EOF.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<usize>> + Send + 'a;

    /// Write every byte of `buf`, or fail.
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a;

    /// Flush any buffered bytes toward the peer.
    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_;
}

/// Produces streams: the client side. `host` is unresolved — name resolution
/// belongs to the adapter (embassy-net DNS, `getaddrinfo`, lwIP).
pub trait StreamDialer {
    /// The stream this dialer produces.
    type Stream: ByteStream + Send;

    /// Open a stream to `host:port`.
    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a;
}

/// Produces streams: the server side.
///
/// One `accept` at a time is the contract, matching
/// [`Listener::accept`](super::Listener::accept) and the `serve` loop that
/// drives it. An adapter backing this with a pool of sockets (embassy-net has
/// no central listener, so each socket must enter `accept()` itself) keeps
/// **all** of them listening inside one `accept` call and returns the first to
/// complete; see `aimdb-embassy-adapter`'s pooled listener.
pub trait StreamListener {
    /// The stream this listener produces.
    type Stream: ByteStream + Send;

    /// Accept the next inbound stream, with whatever peer metadata the
    /// transport exposes.
    fn accept(
        &mut self,
    ) -> impl Future<Output = TransportResult<(Self::Stream, PeerInfo)>> + Send + '_;
}

/// Connectionless I/O — KNX/IP tunnelling, SNTP.
///
/// [`local_addr`](Datagram::local_addr) is not incidental: the KNX tunnelling
/// handshake advertises the client's own endpoint (HPAI), and gateways that
/// reject the NAT-style `0.0.0.0:0` form need the real bound address. A socket
/// is bound by [`DatagramBinder::bind`], so the protocol task can rebind after
/// a socket reset — which the KNX engine's `Action::ResetSocket` requires.
pub trait Datagram {
    /// Send `buf` to `to`.
    fn send_to<'a>(
        &'a mut self,
        buf: &'a [u8],
        to: core::net::SocketAddr,
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a;

    /// Receive one datagram into `buf`, with its source address.
    fn recv_from<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<(usize, core::net::SocketAddr)>> + Send + 'a;

    /// The address this socket is actually bound to, when the stack exposes
    /// one. `None` on stacks that do not (the caller then advertises the
    /// NAT-style endpoint).
    fn local_addr(&self) -> Option<core::net::SocketAddr>;
}

/// Binds [`Datagram`] sockets on demand, so a protocol task can drop and
/// rebind across reconnect cycles instead of holding one socket forever.
pub trait DatagramBinder {
    /// The socket this binder produces.
    type Socket: Datagram + Send;

    /// Bind a socket to `port` (0 = any).
    fn bind(&self, port: u16) -> impl Future<Output = TransportResult<Self::Socket>> + Send + '_;
}

/// A non-allocating sleep.
///
/// [`RuntimeOps::sleep`](crate::executor::RuntimeOps::sleep) is `dyn` and so
/// must box; this one is generic and returns the adapter's own timer type
/// (`embassy_time::Timer`, `tokio::time::Sleep`) with nothing on the heap.
pub trait Delay {
    /// Complete after at least `d` has elapsed.
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send;
}

/// Frames a byte stream: COBS, length-prefix, NDJSON. A transport crate
/// contributes one of these and inherits everything else.
pub trait Framer {
    /// Encode one logical frame, appending its wire bytes to `out`.
    fn encode(&self, frame: &[u8], out: &mut Vec<u8>);
    /// Feed received bytes into the accumulator.
    fn push_bytes(&mut self, bytes: &[u8]);
    /// Pull the next complete frame: `Some(Ok(frame))`, `Some(Err(()))` for a
    /// malformed/unsynced run (skipped, the stream resyncs), or `None`.
    fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>>;
}

/// Builds a fresh [`Framer`] per connection. A blanket impl covers closures,
/// so a caller writes `|| CobsFramer::new()`.
pub trait FramerFactory {
    /// The framer produced.
    type Framer: Framer + Send;
    /// Build one, for one connection.
    fn framer(&self) -> Self::Framer;
}

impl<F, T> FramerFactory for F
where
    F: Fn() -> T,
    T: Framer + Send,
{
    type Framer = T;
    fn framer(&self) -> T {
        self()
    }
}

/// A framed [`Connection`] over any [`ByteStream`] plus any [`Framer`].
///
/// This is the Embassy adapter's `EmbassyConnection` without its
/// `unsafe impl Send`: the force-`Send` moved down into the adapter's stream
/// type, where design 033 already put every other one. `RC` caps the
/// per-`read` chunk; `WC` caps a single `write_all` (some HAL
/// `BufferedUart::write` rejects a write larger than its TX ring).
pub struct FramedConnection<S, F, const RC: usize = 256, const WC: usize = 256> {
    stream: S,
    framer: F,
    peer: PeerInfo,
}

impl<S, F, const RC: usize, const WC: usize> FramedConnection<S, F, RC, WC> {
    /// Frame `stream` with `framer`.
    pub fn new(stream: S, framer: F) -> Self {
        Self {
            stream,
            framer,
            peer: PeerInfo::default(),
        }
    }

    /// Frame `stream` with `framer`, carrying `peer` metadata from the accept.
    pub fn with_peer(stream: S, framer: F, peer: PeerInfo) -> Self {
        Self {
            stream,
            framer,
            peer,
        }
    }

    /// The underlying stream (for a transport that must reach past framing).
    pub fn stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

impl<S, F, const RC: usize, const WC: usize> Connection for FramedConnection<S, F, RC, WC>
where
    S: ByteStream + Send,
    F: Framer + Send,
{
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            loop {
                // A run that fails to decode is line noise or a mid-stream
                // join, not fatal: skip it and resync on the next frame.
                match self.framer.next_frame() {
                    Some(Ok(frame)) => return Ok(Some(frame)),
                    Some(Err(())) => continue,
                    None => {}
                }
                let mut chunk = [0u8; RC];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return Ok(None), // EOF — peer closed
                    Ok(n) => self.framer.push_bytes(&chunk[..n]),
                    Err(e) => return Err(e),
                }
            }
        })
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async move {
            let mut out = Vec::new();
            self.framer.encode(frame, &mut out);
            for chunk in out.chunks(WC) {
                self.stream.write_all(chunk).await?;
            }
            self.stream.flush().await
        })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// Lifts a [`StreamDialer`] + [`FramerFactory`] into the existing
/// [`Dialer`], so `run_client` needs no change.
pub struct FramingDialer<D, FF, const RC: usize = 256, const WC: usize = 256> {
    dialer: D,
    framers: FF,
    host: alloc::string::String,
    port: u16,
}

impl<D, FF, const RC: usize, const WC: usize> FramingDialer<D, FF, RC, WC> {
    /// Dial `host:port` through `dialer`, framing each stream with a framer
    /// from `framers`.
    pub fn new(dialer: D, framers: FF, host: impl Into<alloc::string::String>, port: u16) -> Self {
        Self {
            dialer,
            framers,
            host: host.into(),
            port,
        }
    }
}

impl<D, FF, const RC: usize, const WC: usize> Dialer for FramingDialer<D, FF, RC, WC>
where
    D: StreamDialer + Send + Sync,
    FF: FramerFactory + Send + Sync,
    D::Stream: 'static,
    FF::Framer: 'static,
{
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let stream = self.dialer.connect(&self.host, self.port).await?;
            let conn: FramedConnection<D::Stream, FF::Framer, RC, WC> =
                FramedConnection::new(stream, self.framers.framer());
            Ok(Box::new(conn) as Box<dyn Connection>)
        })
    }
}

/// Lifts a [`StreamListener`] + [`FramerFactory`] into the existing
/// [`Listener`], so `serve` needs no change.
pub struct FramingListener<L, FF, const RC: usize = 256, const WC: usize = 256> {
    listener: L,
    framers: FF,
}

impl<L, FF, const RC: usize, const WC: usize> FramingListener<L, FF, RC, WC> {
    /// Accept through `listener`, framing each stream with a framer from
    /// `framers`.
    pub fn new(listener: L, framers: FF) -> Self {
        Self { listener, framers }
    }
}

impl<L, FF, const RC: usize, const WC: usize> Listener for FramingListener<L, FF, RC, WC>
where
    L: StreamListener + Send,
    FF: FramerFactory + Send,
    L::Stream: 'static,
    FF::Framer: 'static,
{
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let (stream, peer) = self.listener.accept().await?;
            let conn: FramedConnection<L::Stream, FF::Framer, RC, WC> =
                FramedConnection::with_peer(stream, self.framers.framer(), peer);
            Ok(Box::new(conn) as Box<dyn Connection>)
        })
    }
}

/// `TransportError` is the workspace's existing I/O failure type; the design
/// sketch called it `IoError`. Reusing it avoids a parallel error enum and a
/// conversion at every `Connection` boundary.
pub type IoError = TransportError;
