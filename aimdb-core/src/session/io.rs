//! Runtime-neutral byte-stream layer: the seam where an adapter owns sockets,
//! clocks and name resolution, and a connector owns framing and protocol.
//!
//! These traits sit one layer **below** [`Connection`](super::Connection): a
//! [`ByteStream`] is unframed, a [`Connection`](super::Connection) is framed. A
//! connector generic over them needs no per-runtime module and no `cfg` on its
//! code path.
//!
//! Every async method returns `impl Future<…> + Send`. The bound is on the
//! return type because generic code must produce `Send` futures at the boxing
//! boundary and cannot otherwise prove it — return-type notation, which would
//! say exactly that, is still experimental on the pinned toolchain. An
//! implementor whose runtime futures are `!Send` wraps them in a force-`Send`
//! newtype; that `unsafe` belongs to the adapter, and core has none.
//!
//! The traits are deliberately not `dyn`-compatible. Connectors are generic
//! over them; the `dyn` boundary stays at `Box<dyn Connection>` per frame.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;

use super::{BoxFut, Connection, Dialer, Listener, PeerInfo, TransportError, TransportResult};

/// Failure of a byte-level I/O operation.
///
/// An alias, not a new type: these traits sit directly beneath
/// [`Connection`](super::Connection), so nothing converts at that boundary.
pub type IoError = TransportError;

// ===========================================================================
// Byte streams — the one real fork between runtimes.
// ===========================================================================

/// An unframed, bidirectional byte stream — one TCP connection, one UART, one
/// TLS session. The adapter owns it; the connector never names its type.
///
/// `read` returning `Ok(0)` is end of stream, matching both
/// `embedded_io_async::Read` and `tokio::io::AsyncRead`.
///
/// The stream is **unsplit** — one value, `&mut self` on both directions —  so
/// it can wrap a socket that lends out only borrowed halves while a
/// [`Connection`](super::Connection) must own it. Nothing is lost by it:
/// `Connection`'s own `recv`/`send` take `&mut self`, so reads and writes were
/// already serialized.
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

/// Produces streams: the client side.
///
/// `host` is unresolved — name resolution belongs to the adapter, so a
/// connector carries no resolver.
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

/// Produces streams: the server side, and the dual of [`StreamDialer`].
///
/// One `accept` at a time, matching
/// [`Listener::accept`](super::Listener::accept) and the `serve` loop that
/// drives it. The single-shot signature costs no concurrency: an adapter
/// backing it with a pool of sockets keeps every one of them listening across
/// calls and consumes only the one that completes.
pub trait StreamListener {
    /// The stream this listener produces.
    type Stream: ByteStream + Send;

    /// Accept the next inbound stream, with whatever peer metadata the
    /// transport exposes.
    fn accept(
        &mut self,
    ) -> impl Future<Output = TransportResult<(Self::Stream, PeerInfo)>> + Send + '_;
}

// ===========================================================================
// Datagrams.
// ===========================================================================

/// Connectionless I/O.
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

    /// The address this socket is bound to, or `None` on stacks that expose
    /// none. A protocol that advertises its own endpoint in-band needs the real
    /// address rather than the NAT-style `0.0.0.0:0`.
    fn local_addr(&self) -> Option<core::net::SocketAddr>;
}

/// Binds [`Datagram`] sockets on demand.
///
/// A protocol task takes this rather than a socket, because a moved-in socket
/// cannot be rebound after a reset. A std adapter binds a fresh socket; an
/// Embassy one closes and re-binds the same socket, whose buffers live as long
/// as it does.
pub trait DatagramBinder {
    /// The socket this binder produces.
    type Socket: Datagram + Send;

    /// Bind a socket to `port` (`0` for any).
    fn bind(&self, port: u16) -> impl Future<Output = TransportResult<Self::Socket>> + Send + '_;
}

// ===========================================================================
// Time.
// ===========================================================================

/// A non-allocating sleep.
///
/// [`RuntimeOps::sleep`](crate::executor::RuntimeOps::sleep) is `dyn` and must
/// box; this one is generic and returns the adapter's own timer type with
/// nothing on the heap. Only sleeping lives here — `RuntimeOps::now_nanos` is a
/// plain call and stays the clock.
///
/// The returned future borrows nothing, so a task holding a `D: Delay` still
/// produces `'static` futures from it.
pub trait Delay {
    /// Complete after at least `d` has elapsed.
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send;
}

// ===========================================================================
// Framing — a transport crate contributes one of these and inherits the rest.
// ===========================================================================

/// Frames a byte stream: COBS, length-prefix, NDJSON.
pub trait Framer {
    /// Encode one logical frame, appending its wire bytes to `out`.
    fn encode(&self, frame: &[u8], out: &mut Vec<u8>);
    /// Feed received bytes into the accumulator.
    fn push_bytes(&mut self, bytes: &[u8]);
    /// Pull the next complete frame: `Some(Ok(frame))`, `Some(Err(()))` for a
    /// malformed/unsynced run (skipped, the stream resyncs), or `None`.
    fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>>;
}

/// Builds a fresh [`Framer`] per connection.
///
/// A blanket impl covers closures, so a caller writes `|| CobsFramer::new()`.
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

// ===========================================================================
// Framed connections — a [`ByteStream`] plus a [`Framer`] is a [`Connection`].
// ===========================================================================

/// A framed [`Connection`] over any [`ByteStream`] and [`Framer`].
///
/// `RC` caps the per-`read` chunk and `WC` caps a single `write_all`; both are
/// stack buffers, and some HAL `BufferedUart::write` rejects a write larger
/// than its TX ring, which is what `WC` exists for.
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

/// Lifts a [`StreamDialer`] and a [`FramerFactory`] into a [`Dialer`], so
/// `run_client` drives an adapter transport unchanged.
pub struct FramingDialer<D, FF, const RC: usize = 256, const WC: usize = 256> {
    dialer: D,
    framers: FF,
    host: String,
    port: u16,
}

impl<D, FF, const RC: usize, const WC: usize> FramingDialer<D, FF, RC, WC> {
    /// Dial `host:port` through `dialer`, framing each stream with a framer
    /// from `framers`.
    pub fn new(dialer: D, framers: FF, host: impl Into<String>, port: u16) -> Self {
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

/// Lifts a [`StreamListener`] and a [`FramerFactory`] into a [`Listener`], so
/// `serve` drives an adapter transport unchanged.
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

// ===========================================================================
// Moved-in resources.
// ===========================================================================

/// A cell holding a resource until something takes it, once.
///
/// `ConnectorBuilder` is `Send + Sync` and its `build` takes `&self`, so a
/// connector holding a moved-in listener, peripheral or credential set must
/// take it through interior mutability. `spin::Mutex<Option<T>>` is
/// `Send + Sync` whenever `T: Send`, with no `unsafe`.
///
/// The `T: Send` bound is the contract: a type that is not `Send` cannot be
/// held here, and that refusal is the signal to fix the type rather than to
/// reach for `unsafe impl`.
pub struct OneShot<T> {
    inner: spin::Mutex<Option<T>>,
}

impl<T> OneShot<T> {
    /// Hold `value` for a single [`take`](Self::take).
    pub fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(Some(value)),
        }
    }

    /// Take the value, or `None` if it was already taken.
    pub fn take(&self) -> Option<T> {
        self.inner.lock().take()
    }
}

impl<T> Default for OneShot<T> {
    /// An already-empty cell, for a resource that may never be supplied.
    fn default() -> Self {
        Self {
            inner: spin::Mutex::new(None),
        }
    }
}

impl<T> From<T> for OneShot<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

// ===========================================================================
// Compile-time assertions.
// ===========================================================================

/// Generic code over these traits produces `Send` futures, so a connector task
/// built from them boxes as the runner requires. Drop a `+ Send` from any
/// return type above and this stops compiling.
#[allow(dead_code)]
fn _generic_code_over_the_traits_is_send<D, L, B, T>(
    dialer: D,
    mut listener: L,
    binder: B,
    delay: T,
) -> BoxFut<'static, ()>
where
    D: StreamDialer + Send + 'static,
    L: StreamListener + Send + 'static,
    B: DatagramBinder + Send + 'static,
    T: Delay + Send + 'static,
{
    Box::pin(async move {
        let mut buf = [0u8; 16];

        if let Ok(mut stream) = dialer.connect("example.test", 1883).await {
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(&buf).await;
            let _ = stream.flush().await;
        }
        if let Ok((mut stream, _peer)) = listener.accept().await {
            let _ = stream.read(&mut buf).await;
        }
        if let Ok(mut socket) = binder.bind(0).await {
            let _ = socket.recv_from(&mut buf).await;
            let _ = socket.local_addr();
        }
        delay.sleep(core::time::Duration::from_millis(1)).await;
    })
}

/// `OneShot<T>` is `Send + Sync` for any `T: Send`, with no `unsafe`.
#[allow(dead_code)]
fn _one_shot_is_send_sync_without_unsafe() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OneShot<Vec<u8>>>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;

    // --- Test doubles -----------------------------------------------------

    /// Length-prefixed framer: one length byte, then that many payload bytes.
    /// A `0xFF` length marks a corrupt run, so resync has something to skip.
    #[derive(Default)]
    struct LenFramer {
        buf: Vec<u8>,
    }

    impl Framer for LenFramer {
        fn encode(&self, frame: &[u8], out: &mut Vec<u8>) {
            out.push(frame.len() as u8);
            out.extend_from_slice(frame);
        }
        fn push_bytes(&mut self, bytes: &[u8]) {
            self.buf.extend_from_slice(bytes);
        }
        fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>> {
            let len = *self.buf.first()? as usize;
            if len == 0xFF {
                self.buf.remove(0);
                return Some(Err(()));
            }
            if self.buf.len() < len + 1 {
                return None;
            }
            let frame = self.buf[1..len + 1].to_vec();
            self.buf.drain(..len + 1);
            Some(Ok(frame))
        }
    }

    /// Shared so a test can inspect what the connection wrote after moving the
    /// stream into it.
    #[derive(Default)]
    struct StreamState {
        /// Chunks `read` hands out, in order; empty means EOF.
        reads: Vec<Vec<u8>>,
        /// Bytes written, concatenated.
        written: Vec<u8>,
        /// Size of each individual `write_all` call.
        write_sizes: Vec<usize>,
        flushes: usize,
    }

    #[derive(Clone, Default)]
    struct MockStream(Arc<spin::Mutex<StreamState>>);

    impl MockStream {
        fn with_reads(reads: Vec<Vec<u8>>) -> Self {
            Self(Arc::new(spin::Mutex::new(StreamState {
                reads,
                ..Default::default()
            })))
        }
    }

    // Written as plain `async fn`s: an impl on a runtime whose futures are
    // already `Send` needs nothing more, and the compiler checks the bound the
    // trait declares.
    impl ByteStream for MockStream {
        async fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> TransportResult<usize> {
            let mut st = self.0.lock();
            if st.reads.is_empty() {
                return Ok(0);
            }
            let chunk = st.reads.remove(0);
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            Ok(n)
        }

        async fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> TransportResult<()> {
            let mut st = self.0.lock();
            st.written.extend_from_slice(buf);
            st.write_sizes.push(buf.len());
            Ok(())
        }

        async fn flush(&mut self) -> TransportResult<()> {
            self.0.lock().flushes += 1;
            Ok(())
        }
    }

    /// A stream whose first read fails, to check the error is propagated as-is
    /// rather than flattened.
    struct FailingStream;

    impl ByteStream for FailingStream {
        async fn read<'a>(&'a mut self, _buf: &'a mut [u8]) -> TransportResult<usize> {
            Err(TransportError::Closed)
        }
        async fn write_all<'a>(&'a mut self, _buf: &'a [u8]) -> TransportResult<()> {
            Err(TransportError::Closed)
        }
        async fn flush(&mut self) -> TransportResult<()> {
            Ok(())
        }
    }

    struct MockDialer(MockStream);

    impl StreamDialer for MockDialer {
        type Stream = MockStream;
        async fn connect<'a>(&'a self, _host: &'a str, _port: u16) -> TransportResult<MockStream> {
            Ok(self.0.clone())
        }
    }

    struct MockListener(Option<MockStream>);

    impl StreamListener for MockListener {
        type Stream = MockStream;
        async fn accept(&mut self) -> TransportResult<(MockStream, PeerInfo)> {
            let s = self.0.take().ok_or(TransportError::Closed)?;
            let peer = PeerInfo {
                peer_addr: Some("10.0.0.1:5555".into()),
                ..Default::default()
            };
            Ok((s, peer))
        }
    }

    fn framed(stream: MockStream) -> FramedConnection<MockStream, LenFramer, 256, 256> {
        FramedConnection::new(stream, LenFramer::default())
    }

    // --- FramedConnection -------------------------------------------------

    #[tokio::test]
    async fn recv_yields_each_frame_then_eof() {
        let mut conn = framed(MockStream::with_reads(vec![vec![
            2, b'h', b'i', 3, b'y', b'e', b's',
        ]]));
        assert_eq!(conn.recv().await.unwrap(), Some(b"hi".to_vec()));
        assert_eq!(conn.recv().await.unwrap(), Some(b"yes".to_vec()));
        assert_eq!(conn.recv().await.unwrap(), None, "closed peer is Ok(None)");
    }

    #[tokio::test]
    async fn recv_reassembles_a_frame_split_across_reads() {
        let mut conn = framed(MockStream::with_reads(vec![
            vec![3, b'a'],
            vec![b'b'],
            vec![b'c'],
        ]));
        assert_eq!(conn.recv().await.unwrap(), Some(b"abc".to_vec()));
    }

    #[tokio::test]
    async fn recv_skips_a_corrupt_run_and_resyncs() {
        let mut conn = framed(MockStream::with_reads(vec![vec![
            0xFF, 0xFF, 2, b'o', b'k',
        ]]));
        assert_eq!(
            conn.recv().await.unwrap(),
            Some(b"ok".to_vec()),
            "a run that fails to decode is skipped, not fatal"
        );
    }

    #[tokio::test]
    async fn recv_propagates_the_streams_own_error() {
        let mut conn = FramedConnection::<_, _, 256, 256>::new(FailingStream, LenFramer::default());
        assert_eq!(
            conn.recv().await,
            Err(TransportError::Closed),
            "the stream classifies the failure; framing must not flatten it"
        );
    }

    #[tokio::test]
    async fn send_encodes_then_flushes() {
        let stream = MockStream::default();
        let mut conn = framed(stream.clone());
        conn.send(b"hi").await.unwrap();

        let st = stream.0.lock();
        assert_eq!(st.written, vec![2, b'h', b'i']);
        assert_eq!(st.flushes, 1);
    }

    #[tokio::test]
    async fn send_splits_a_frame_larger_than_the_write_chunk() {
        let stream = MockStream::default();
        let mut conn: FramedConnection<MockStream, LenFramer, 256, 4> =
            FramedConnection::new(stream.clone(), LenFramer::default());
        conn.send(b"0123456789").await.unwrap();

        let st = stream.0.lock();
        assert_eq!(st.write_sizes, vec![4, 4, 3], "11 encoded bytes at WC = 4");
        assert_eq!(st.written.len(), 11);
    }

    // --- FramingDialer / FramingListener ----------------------------------

    #[tokio::test]
    async fn framing_dialer_produces_a_working_connection() {
        let stream = MockStream::with_reads(vec![vec![2, b'h', b'i']]);
        let dialer: FramingDialer<_, _, 256, 256> =
            FramingDialer::new(MockDialer(stream), LenFramer::default, "host.test", 1883);

        let mut conn = dialer.connect().await.unwrap();
        assert_eq!(conn.recv().await.unwrap(), Some(b"hi".to_vec()));
    }

    #[tokio::test]
    async fn framing_listener_carries_peer_metadata_through() {
        let stream = MockStream::with_reads(vec![vec![2, b'h', b'i']]);
        let mut listener: FramingListener<_, _, 256, 256> =
            FramingListener::new(MockListener(Some(stream)), LenFramer::default);

        let mut conn = listener.accept().await.unwrap();
        assert_eq!(conn.peer().peer_addr.as_deref(), Some("10.0.0.1:5555"));
        assert_eq!(conn.recv().await.unwrap(), Some(b"hi".to_vec()));
    }

    #[test]
    fn a_framed_connection_is_boxable_as_dyn_connection() {
        let conn = framed(MockStream::default());
        let _boxed: Box<dyn Connection> = Box::new(conn);
    }

    // --- OneShot / FramerFactory ------------------------------------------

    #[test]
    fn one_shot_yields_its_value_once() {
        let cell = OneShot::new(7u32);
        assert_eq!(cell.take(), Some(7));
        assert_eq!(cell.take(), None, "a second take must not get the resource");
    }

    #[test]
    fn default_one_shot_is_empty() {
        assert_eq!(OneShot::<u32>::default().take(), None);
    }

    #[test]
    fn framer_factory_is_implemented_for_closures() {
        struct Noop;
        impl Framer for Noop {
            fn encode(&self, _frame: &[u8], _out: &mut Vec<u8>) {}
            fn push_bytes(&mut self, _bytes: &[u8]) {}
            fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>> {
                None
            }
        }

        fn takes_factory<FF: FramerFactory>(ff: FF) -> FF::Framer {
            ff.framer()
        }
        let _framer = takes_factory(|| Noop);
    }
}
