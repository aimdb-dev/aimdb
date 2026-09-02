//! Embassy implementations of core's runtime-neutral I/O traits (design 052
//! §3.2): the adapter owns sockets and clocks, the connector owns framing and
//! protocol.
//!
//! Everything here is the Embassy side of
//! [`aimdb_core::session`]'s [`ByteStream`] / [`StreamDialer`] /
//! [`StreamListener`] / [`Delay`]. The single-core `unsafe` stays exactly where
//! design 033 put it — this module — and connector crates carry none.
//!
//! # Safety invariant
//!
//! Same as [`crate::connectors`]: an Embassy executor runs cooperatively on a
//! single core with no preemption or thread migration, so the wrapped `!Send`
//! values are never actually touched from another thread.

use core::cell::RefCell;
use core::future::poll_fn;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;

use aimdb_core::session::{
    ByteStream, Datagram, DatagramBinder, PeerInfo, StreamDialer, StreamListener, TransportError,
    TransportResult,
};

use embassy_net::tcp::TcpSocket;
use embassy_net::udp::UdpSocket;
use embassy_net::{IpEndpoint, IpListenEndpoint, Stack};
use embedded_io_async::Write as _;

use crate::SendFutureWrapper;

// ===========================================================================
// Socket slot — moved verbatim from `aimdb-tcp-connector::embassy_transport`,
// where it was the connector's problem. It is the adapter's now.
// ===========================================================================

/// Holds one reusable `embassy-net` TCP socket between uses.
///
/// `embassy-net` has no central listener: every socket must enter `accept()`
/// itself, and a socket can be reused after `abort()`. The slot is what lets a
/// dropped [`EmbassyTcpStream`] hand its socket back for the next accept.
pub struct TcpSocketSlot {
    socket: RefCell<Option<TcpSocket<'static>>>,
    // Single `Waker`: at most one accept ever waits on a given slot — the
    // pooled listener drives exactly one pending accept per slot.
    waker: RefCell<Option<Waker>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for TcpSocketSlot {}
// SAFETY: same invariant; shared only so a dropped stream can return its socket.
unsafe impl Sync for TcpSocketSlot {}

impl TcpSocketSlot {
    /// Hold `socket` for the next taker.
    pub fn new(socket: TcpSocket<'static>) -> Self {
        Self {
            socket: RefCell::new(Some(socket)),
            waker: RefCell::new(None),
        }
    }

    /// Take the socket if it is free right now.
    pub fn take(&self) -> Option<TcpSocket<'static>> {
        self.socket.borrow_mut().take()
    }

    /// Wait until the socket is free, then take it.
    pub async fn acquire(&self) -> TcpSocket<'static> {
        poll_fn(|cx| self.poll_take(cx)).await
    }

    fn poll_take(&self, cx: &mut Context<'_>) -> Poll<TcpSocket<'static>> {
        let mut slot = self.socket.borrow_mut();
        if let Some(socket) = slot.take() {
            Poll::Ready(socket)
        } else {
            drop(slot);
            *self.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// Return the socket to the slot, waking whoever is waiting for it.
    pub fn put(&self, socket: TcpSocket<'static>) {
        let mut slot = self.socket.borrow_mut();
        debug_assert!(slot.is_none(), "Embassy TCP socket returned twice");
        if slot.is_none() {
            *slot = Some(socket);
        }
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

// ===========================================================================
// ByteStream over one embassy-net TCP socket.
// ===========================================================================

/// One `embassy-net` TCP connection as a runtime-neutral [`ByteStream`].
///
/// Owns its socket (embassy-net's `split()` only yields *borrowed* halves,
/// which is why the framed connection has to be unsplit) and returns it to its
/// slot on drop, so the pool can re-accept on it.
pub struct EmbassyTcpStream {
    socket: Option<TcpSocket<'static>>,
    recycler: Option<Arc<TcpSocketSlot>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
// This is the one force-`Send` the whole TCP path needs; the connector has none.
unsafe impl Send for EmbassyTcpStream {}

impl EmbassyTcpStream {
    fn recyclable(socket: TcpSocket<'static>, recycler: Arc<TcpSocketSlot>) -> Self {
        Self {
            socket: Some(socket),
            recycler: Some(recycler),
        }
    }

    fn socket_mut(&mut self) -> TransportResult<&mut TcpSocket<'static>> {
        self.socket.as_mut().ok_or(TransportError::Closed)
    }
}

impl Drop for EmbassyTcpStream {
    fn drop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            // Double abort is intentional: this resets the link promptly on
            // drop; the next taker re-aborts before reuse for a clean socket.
            socket.abort();
            if let Some(recycler) = &self.recycler {
                recycler.put(socket);
            }
        }
    }
}

impl ByteStream for EmbassyTcpStream {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<usize>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket.read(buf).await.map_err(|_| TransportError::Io)
        })
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket
                .write_all(buf)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_ {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket.flush().await.map_err(|_| TransportError::Closed)
        })
    }
}

// ===========================================================================
// Dialer.
// ===========================================================================

/// A reusable Embassy TCP dialer over one caller-owned socket.
pub struct EmbassyTcpDialer {
    slot: Arc<TcpSocketSlot>,
}

impl StreamDialer for EmbassyTcpDialer {
    type Stream = EmbassyTcpStream;

    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a {
        SendFutureWrapper(async move {
            // Name resolution is the adapter's job. This build resolves IP
            // literals only; with embassy-net's `dns` feature the same seam
            // takes a hostname, which the connector never has to know about.
            let addr: core::net::IpAddr = host.parse().map_err(|_| TransportError::Io)?;
            let endpoint = IpEndpoint::new(addr.into(), port);

            let Some(mut socket) = self.slot.take() else {
                return Err(TransportError::Io);
            };
            socket.abort();
            match socket.connect(endpoint).await {
                Ok(()) => Ok(EmbassyTcpStream::recyclable(socket, self.slot.clone())),
                Err(_) => {
                    socket.abort();
                    self.slot.put(socket);
                    Err(TransportError::Io)
                }
            }
        })
    }
}

// ===========================================================================
// Pooled listener — the answer to "can `StreamListener` back the N-socket pool".
// ===========================================================================

/// The accept future for one slot: owns the socket for the duration, so it is
/// `'static` and can be **stored** in the listener between `accept` calls.
type PendingAccept = Pin<Box<dyn Future<Output = TransportResult<TcpSocket<'static>>>>>;

/// An Embassy TCP listener over `N` caller-owned sockets, exposed through the
/// single-accept [`StreamListener`] contract.
///
/// # Why this keeps all `N` sockets listening
///
/// The naive reading is that a one-at-a-time `accept(&mut self)` can only hold
/// one socket in `accept()`, losing the pool's whole point. It does not, for
/// one reason: each slot's accept future is created once and **stored**, so a
/// call that returns slot *i*'s connection leaves the other `N-1` futures
/// pending and untouched, still in smoltcp's `Listen` state. Nothing is
/// cancelled and no socket ever leaves `LISTEN` between calls, so a SYN that
/// arrives while the caller is between accepts still lands.
///
/// That matters because `TcpSocket::accept` is a synchronous `listen()`
/// followed by a bare `poll_fn`: re-entering it on a socket that is already
/// listening is an error, and `abort()`ing to make it re-enterable is exactly
/// what would open the window. Storing the futures avoids the question.
pub struct EmbassyTcpListener<const N: usize> {
    local_endpoint: IpListenEndpoint,
    slots: [Arc<TcpSocketSlot>; N],
    pending: [Option<PendingAccept>; N],
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl<const N: usize> Send for EmbassyTcpListener<N> {}

impl<const N: usize> EmbassyTcpListener<N> {
    /// Arm slot `i`'s accept if it is not already in flight.
    fn arm(&mut self, i: usize) {
        if self.pending[i].is_some() {
            return;
        }
        let slot = self.slots[i].clone();
        let endpoint = self.local_endpoint;
        self.pending[i] = Some(Box::pin(async move {
            // Wait for the socket to be back in its slot (a previous
            // connection may still hold it), then listen on it.
            let mut socket = slot.acquire().await;
            socket.abort();
            match socket.accept(endpoint).await {
                Ok(()) => Ok(socket),
                Err(_) => {
                    socket.abort();
                    slot.put(socket);
                    Err(TransportError::Io)
                }
            }
        }));
    }
}

impl<const N: usize> StreamListener for EmbassyTcpListener<N> {
    type Stream = EmbassyTcpStream;

    fn accept(
        &mut self,
    ) -> impl Future<Output = TransportResult<(Self::Stream, PeerInfo)>> + Send + '_ {
        SendFutureWrapper(async move {
            for i in 0..N {
                self.arm(i);
            }
            poll_fn(|cx| {
                for i in 0..N {
                    let Some(fut) = self.pending[i].as_mut() else {
                        continue;
                    };
                    match fut.as_mut().poll(cx) {
                        Poll::Ready(result) => {
                            // Only this slot's future is consumed; the other
                            // N-1 stay pending and keep their sockets in LISTEN.
                            self.pending[i] = None;
                            let socket = match result {
                                Ok(socket) => socket,
                                Err(e) => return Poll::Ready(Err(e)),
                            };
                            // `PeerInfo` is `#[non_exhaustive]`, so build it
                            // by mutation rather than a struct expression.
                            let mut peer = PeerInfo::default();
                            peer.peer_addr = socket.remote_endpoint().map(|e| e.to_string());
                            let stream =
                                EmbassyTcpStream::recyclable(socket, self.slots[i].clone());
                            return Poll::Ready(Ok((stream, peer)));
                        }
                        Poll::Pending => {}
                    }
                }
                Poll::Pending
            })
            .await
        })
    }
}


// ===========================================================================
// Datagram — KNX/IP tunnelling and SNTP.
// ===========================================================================

/// Holds the one reusable `embassy-net` UDP socket between binds.
///
/// `UdpSocket` owns its buffers for its whole lifetime, so a rebind cannot
/// recreate the socket without leaking them. It does not have to: `close()`
/// followed by `bind()` returns the same socket to a fresh unbound state,
/// which is exactly what the KNX engine's `Action::ResetSocket` needs.
struct UdpSlot {
    socket: RefCell<Option<UdpSocket<'static>>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for UdpSlot {}
// SAFETY: same invariant.
unsafe impl Sync for UdpSlot {}

/// One bound `embassy-net` UDP socket as a runtime-neutral [`Datagram`].
pub struct EmbassyUdpSocket {
    socket: Option<UdpSocket<'static>>,
    slot: Arc<UdpSlot>,
    local: Option<core::net::SocketAddr>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for EmbassyUdpSocket {}

impl Drop for EmbassyUdpSocket {
    fn drop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            socket.close();
            *self.slot.socket.borrow_mut() = Some(socket);
        }
    }
}

fn to_endpoint(addr: core::net::SocketAddr) -> IpEndpoint {
    IpEndpoint::new(addr.ip().into(), addr.port())
}

impl Datagram for EmbassyUdpSocket {
    fn send_to<'a>(
        &'a mut self,
        buf: &'a [u8],
        to: core::net::SocketAddr,
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket.as_mut().ok_or(TransportError::Closed)?;
            socket
                .send_to(buf, to_endpoint(to))
                .await
                .map_err(|_| TransportError::Io)
        })
    }

    fn recv_from<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<(usize, core::net::SocketAddr)>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket.as_mut().ok_or(TransportError::Closed)?;
            let (n, meta) = socket.recv_from(buf).await.map_err(|_| TransportError::Io)?;
            let addr = core::net::SocketAddr::new(meta.endpoint.addr.into(), meta.endpoint.port);
            Ok((n, addr))
        })
    }

    /// The real bound address, assembled from the socket's port and the
    /// stack's configured IPv4 address.
    ///
    /// This is what lets the KNX tunnelling handshake advertise
    /// `LocalEndpoint::Explicit` on Embassy, which the hand-written Embassy
    /// client never did — gateways that reject the NAT-style `0.0.0.0:0` HPAI
    /// work through this path.
    fn local_addr(&self) -> Option<core::net::SocketAddr> {
        self.local
    }
}

/// Binds [`EmbassyUdpSocket`]s over one caller-owned socket.
pub struct EmbassyUdpBinder {
    stack: Stack<'static>,
    slot: Arc<UdpSlot>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for EmbassyUdpBinder {}
// SAFETY: same invariant.
unsafe impl Sync for EmbassyUdpBinder {}

impl DatagramBinder for EmbassyUdpBinder {
    type Socket = EmbassyUdpSocket;

    fn bind(&self, port: u16) -> impl Future<Output = TransportResult<Self::Socket>> + Send + '_ {
        SendFutureWrapper(async move {
            let mut socket = self
                .slot
                .socket
                .borrow_mut()
                .take()
                .ok_or(TransportError::Io)?;
            // Idempotent: a socket returned by a dropped `EmbassyUdpSocket` is
            // already closed, and closing an unbound socket is a no-op.
            socket.close();
            if socket.bind(port).is_err() {
                *self.slot.socket.borrow_mut() = Some(socket);
                return Err(TransportError::Io);
            }
            let bound_port = socket.endpoint().port;
            let local = self.stack.config_v4().map(|cfg| {
                core::net::SocketAddr::new(
                    core::net::IpAddr::V4(cfg.address.address().into()),
                    bound_port,
                )
            });
            Ok(EmbassyUdpSocket {
                socket: Some(socket),
                slot: self.slot.clone(),
                local,
            })
        })
    }
}

// ===========================================================================
// Constructors + clock.
// ===========================================================================

/// Embassy network entry point: hands connectors transports, keeping
/// `embassy_net::Stack` (and its `unsafe`) inside the adapter.
pub struct EmbassyNet;

impl EmbassyNet {
    /// A reusable TCP dialer over caller-owned socket buffers.
    pub fn tcp(
        stack: Stack<'static>,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> EmbassyTcpDialer {
        EmbassyTcpDialer {
            slot: Arc::new(TcpSocketSlot::new(TcpSocket::new(stack, rx_buffer, tx_buffer))),
        }
    }

    /// A UDP binder over one caller-owned socket, for KNX/IP and SNTP.
    pub fn udp(
        stack: Stack<'static>,
        rx_meta: &'static mut [embassy_net::udp::PacketMetadata],
        rx_buffer: &'static mut [u8],
        tx_meta: &'static mut [embassy_net::udp::PacketMetadata],
        tx_buffer: &'static mut [u8],
    ) -> EmbassyUdpBinder {
        EmbassyUdpBinder {
            stack,
            slot: Arc::new(UdpSlot {
                socket: RefCell::new(Some(UdpSocket::new(
                    stack, rx_meta, rx_buffer, tx_meta, tx_buffer,
                ))),
            }),
        }
    }

    /// An `N`-socket listener pool on `local_endpoint`, each socket backed by
    /// one caller-owned RX/TX buffer pair.
    pub fn listen<const N: usize>(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx_buffers: [&'static mut [u8]; N],
        tx_buffers: [&'static mut [u8]; N],
    ) -> EmbassyTcpListener<N> {
        let mut rx = rx_buffers.into_iter();
        let mut tx = tx_buffers.into_iter();
        let slots = core::array::from_fn(|_| {
            let rx = rx.next().expect("array iterator yields exactly N buffers");
            let tx = tx.next().expect("array iterator yields exactly N buffers");
            Arc::new(TcpSocketSlot::new(TcpSocket::new(stack, rx, tx)))
        });
        EmbassyTcpListener {
            local_endpoint: local_endpoint.into(),
            slots,
            pending: core::array::from_fn(|_| None),
        }
    }
}

/// [`Delay`] over `embassy_time::Timer` — a two-field struct, `Send`, no heap.
/// Contrast `RuntimeOps::sleep`, which is `dyn` and boxes per call.
#[cfg(feature = "embassy-time")]
#[derive(Clone, Copy, Default)]
pub struct EmbassyDelay;

#[cfg(feature = "embassy-time")]
impl aimdb_core::session::Delay for EmbassyDelay {
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send {
        embassy_time::Timer::after(embassy_time::Duration::from_micros(d.as_micros() as u64))
    }
}
