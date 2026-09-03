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
use alloc::vec::Vec;
use core::future::Future;

use super::{BoxFut, PeerInfo, TransportError, TransportResult};

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
