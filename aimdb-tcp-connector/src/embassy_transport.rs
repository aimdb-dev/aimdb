//! Embassy TCP transport (feature `embassy-runtime`).
//!
//! `embassy-net` has no central `TcpListener`; each `TcpSocket` must enter
//! `accept()` itself. This module therefore models Embassy TCP servers as an
//! explicit pool of caller-buffered sockets, with one accept/session worker per
//! active slot.
//! `connector-io` cannot be reused directly because `TcpSocket::split()` only
//! yields borrowed halves, while AimDB's `Connection` must own the socket.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{
    run_session, BoxFut, ClientConfig, Connection, Dialer, Dispatch, Listener, PeerInfo,
    SessionConfig, SessionLimits, TransportError, TransportResult,
};
use aimdb_embassy_adapter::connectors::{EmbassySessionClient, OneShotCell};
use aimdb_embassy_adapter::SendFutureWrapper;
use embassy_futures::yield_now;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpEndpoint, IpListenEndpoint, Stack};
use embedded_io_async::Write;

use aimdb_core::{AimDb, DbResult};

use crate::framing::{encode_frame, FrameAccumulator, HEADER_LEN};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

const READ_CHUNK: usize = 256;

/// A framed AimX connection over one `embassy-net` TCP socket.
pub struct TcpConnection {
    socket: Option<TcpSocket<'static>>,
    recycler: Option<Arc<TcpSocketSlot>>,
    acc: FrameAccumulator,
    peer: PeerInfo,
}

// SAFETY: single-core cooperative Embassy executor; same invariant as
// `aimdb-embassy-adapter::connectors`.
unsafe impl Send for TcpConnection {}

impl TcpConnection {
    /// Wrap an already-connected TCP socket.
    pub fn new(socket: TcpSocket<'static>) -> Self {
        Self {
            socket: Some(socket),
            recycler: None,
            acc: FrameAccumulator::new(),
            peer: PeerInfo::default(),
        }
    }

    fn reusable(socket: TcpSocket<'static>, recycler: Arc<TcpSocketSlot>) -> Self {
        Self {
            socket: Some(socket),
            recycler: Some(recycler),
            acc: FrameAccumulator::new(),
            peer: PeerInfo::default(),
        }
    }
}

impl Connection for TcpConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(SendFutureWrapper(async move {
            let socket = self.socket.as_mut().ok_or(TransportError::Closed)?;
            loop {
                match self.acc.next_frame() {
                    Some(Ok(frame)) => return Ok(Some(frame)),
                    Some(Err(_)) => return Err(TransportError::Io),
                    None => {}
                }

                let mut chunk = [0u8; READ_CHUNK];
                match socket.read(&mut chunk).await {
                    Ok(0) => return Ok(None),
                    Ok(n) => self.acc.push_bytes(&chunk[..n]),
                    Err(_) => return Err(TransportError::Io),
                }
            }
        }))
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(SendFutureWrapper(async move {
            let socket = self.socket.as_mut().ok_or(TransportError::Closed)?;
            let capacity = HEADER_LEN
                .checked_add(frame.len())
                .ok_or(TransportError::Io)?;
            let mut out = Vec::with_capacity(capacity);
            encode_frame(frame, &mut out).map_err(|_| TransportError::Io)?;
            write_all(socket, &out).await?;
            socket.flush().await.map_err(|_| TransportError::Closed)
        }))
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

impl Drop for TcpConnection {
    fn drop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            // Double abort is intentional: this one resets the link promptly on
            // drop; the next taker re-aborts before reuse for a clean socket.
            socket.abort();
            if let Some(recycler) = &self.recycler {
                recycler.put(socket);
            }
        }
    }
}

async fn write_all<W>(writer: &mut W, mut bytes: &[u8]) -> TransportResult<()>
where
    W: Write,
{
    while !bytes.is_empty() {
        let n = writer
            .write(bytes)
            .await
            .map_err(|_| TransportError::Closed)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        bytes = &bytes[n..];
    }
    Ok(())
}

struct TcpSocketSlot {
    socket: RefCell<Option<TcpSocket<'static>>>,
    waker: RefCell<Option<Waker>>,
}

// SAFETY: single-core cooperative Embassy executor; the socket and stack stay on
// the same executor task set, and `RefCell` is never borrowed from another core.
unsafe impl Send for TcpSocketSlot {}
// SAFETY: same invariant. Shared only so a dropped connection can return its
// socket to the slot — owned by a dialer, an accept/session worker, or the
// `Listener` compat path, never touched from more than one at a time.
unsafe impl Sync for TcpSocketSlot {}

impl TcpSocketSlot {
    fn new(socket: TcpSocket<'static>) -> Self {
        Self {
            socket: RefCell::new(Some(socket)),
            waker: RefCell::new(None),
        }
    }

    fn take(&self) -> Option<TcpSocket<'static>> {
        self.socket.borrow_mut().take()
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

    fn put(&self, socket: TcpSocket<'static>) {
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

/// A reusable Embassy TCP dialer backed by one caller-owned socket.
///
/// Unlike moved-in UART peripherals, `embassy-net` TCP sockets can be reused
/// after `abort()`/`close()`, so this dialer can redial with the same static
/// RX/TX buffers.
pub struct TcpDialer {
    endpoint: IpEndpoint,
    socket: Arc<TcpSocketSlot>,
}

impl TcpDialer {
    /// Build a reusable dialer with caller-owned socket buffers.
    pub fn new(
        stack: Stack<'static>,
        endpoint: IpEndpoint,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> Self {
        let socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        Self {
            endpoint,
            socket: Arc::new(TcpSocketSlot::new(socket)),
        }
    }
}

impl Dialer for TcpDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            let Some(mut socket) = self.socket.take() else {
                return Err(TransportError::Io);
            };
            socket.abort();
            match socket.connect(self.endpoint).await {
                Ok(()) => Ok(
                    Box::new(TcpConnection::reusable(socket, self.socket.clone()))
                        as Box<dyn Connection>,
                ),
                Err(_) => {
                    socket.abort();
                    self.socket.put(socket);
                    Err(TransportError::Io)
                }
            }
        }))
    }
}

/// An Embassy TCP listener backed by `N` caller-owned sockets.
///
/// `embassy-net` requires each `TcpSocket` to enter `accept()` itself, so true
/// concurrent listening means keeping multiple sockets in accept state at once.
/// `TcpServer::<N>::with_buffers(...)` drives this listener with one worker per
/// enabled slot. The `Listener` trait implementation is only for the `N = 1`
/// compatibility path.
pub struct TcpListener<const N: usize = 1> {
    local_endpoint: IpListenEndpoint,
    slots: [Arc<TcpSocketSlot>; N],
}

impl TcpListener<1> {
    /// Build a single-socket listener with caller-owned socket buffers.
    pub fn new(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> Self {
        Self::with_buffers(stack, local_endpoint, [rx_buffer], [tx_buffer])
    }
}

impl<const N: usize> TcpListener<N> {
    /// Build an N-socket listener pool with caller-owned socket buffers.
    ///
    /// Each `(rx_buffers[i], tx_buffers[i])` pair backs one `TcpSocket` and one
    /// concurrent accept/session worker. Keep `N` aligned with the
    /// `embassy-net::StackResources<SOCK>` capacity and the RAM budget for the
    /// chosen RX/TX buffer sizes.
    pub fn with_buffers(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx_buffers: [&'static mut [u8]; N],
        tx_buffers: [&'static mut [u8]; N],
    ) -> Self {
        let mut rx_buffers = rx_buffers.into_iter();
        let mut tx_buffers = tx_buffers.into_iter();
        let slots = core::array::from_fn(|_| {
            let rx = rx_buffers
                .next()
                .expect("array iterator yields exactly N RX buffers");
            let tx = tx_buffers
                .next()
                .expect("array iterator yields exactly N TX buffers");
            Arc::new(TcpSocketSlot::new(TcpSocket::new(stack, rx, tx)))
        });
        Self {
            local_endpoint: local_endpoint.into(),
            slots,
        }
    }

    fn into_server_futures(
        self,
        codec: Arc<AimxCodec>,
        dispatch: Arc<dyn Dispatch>,
        config: SessionConfig,
        max_workers: usize,
    ) -> Vec<BoxFuture> {
        let worker_count = max_workers.min(N);
        let mut futures = Vec::with_capacity(worker_count);
        for slot in self.slots.into_iter().take(worker_count) {
            futures.push(Box::pin(SendFutureWrapper(serve_socket_slot(
                slot,
                self.local_endpoint,
                codec.clone(),
                dispatch.clone(),
                config.clone(),
            ))) as BoxFuture);
        }
        futures
    }
}

impl Listener for TcpListener<1> {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            let slot = &self.slots[0];
            let mut socket = poll_fn(|cx| slot.poll_take(cx)).await;
            socket.abort();
            match socket.accept(self.local_endpoint).await {
                Ok(()) => {
                    Ok(Box::new(TcpConnection::reusable(socket, slot.clone()))
                        as Box<dyn Connection>)
                }
                Err(_) => {
                    socket.abort();
                    slot.put(socket);
                    // Same synchronous-failure guard as `serve_socket_slot`:
                    // `serve()` re-enters `accept()` immediately on `Err`, so yield.
                    yield_now().await;
                    Err(TransportError::Io)
                }
            }
        }))
    }
}

async fn serve_socket_slot(
    slot: Arc<TcpSocketSlot>,
    local_endpoint: IpListenEndpoint,
    codec: Arc<AimxCodec>,
    dispatch: Arc<dyn Dispatch>,
    config: SessionConfig,
) {
    loop {
        let mut socket = poll_fn(|cx| slot.poll_take(cx)).await;
        socket.abort();
        match socket.accept(local_endpoint).await {
            Ok(()) => {
                let conn =
                    Box::new(TcpConnection::reusable(socket, slot.clone())) as Box<dyn Connection>;
                run_session(conn, codec.as_ref(), dispatch.as_ref(), &config).await;
            }
            Err(_) => {
                socket.abort();
                slot.put(socket);
                // `accept()` can fail synchronously (e.g. port-0 `InvalidPort`);
                // without this await the loop retakes the slot and retries with no
                // yield point, starving the executor. Yield so a misconfig
                // warn-loops instead of hanging.
                yield_now().await;
            }
        }
    }
}

/// Constructs an Embassy session client over TCP.
pub struct TcpClient;

impl TcpClient {
    /// Mirror records to/from an AimX peer over TCP.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        stack: Stack<'static>,
        endpoint: IpEndpoint,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> EmbassySessionClient<TcpDialer, AimxCodec> {
        EmbassySessionClient::new(
            TcpDialer::new(stack, endpoint, rx_buffer, tx_buffer),
            AimxCodec,
        )
        .scheme(DEFAULT_SCHEME)
        .with_config(ClientConfig::default())
    }
}

/// Accepts AimX connections over an explicit Embassy TCP socket pool.
///
/// `TcpServer::new(...)` is the one-socket convenience constructor. Use
/// `TcpServer::<N>::with_buffers(...)` to keep `N` sockets concurrently
/// listening, each backed by caller-owned static RX/TX buffers.
pub struct TcpServer<const N: usize = 1> {
    listener: OneShotCell<TcpListener<N>>,
    config: AimxConfig,
    scheme: String,
}

impl TcpServer<1> {
    /// Serve AimX on one Embassy TCP socket.
    pub fn new(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> Self {
        Self {
            listener: OneShotCell::new(TcpListener::new(
                stack,
                local_endpoint,
                rx_buffer,
                tx_buffer,
            )),
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }
}

impl<const N: usize> TcpServer<N> {
    /// Serve AimX over an N-socket Embassy TCP listener pool.
    ///
    /// The server starts up to `min(N, max_connections)` workers. Each worker
    /// keeps one `TcpSocket` in `accept()` while idle, so the network stack has
    /// multiple pending listeners instead of rejecting inbound SYNs after a
    /// single socket is consumed.
    pub fn with_buffers(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx_buffers: [&'static mut [u8]; N],
        tx_buffers: [&'static mut [u8]; N],
    ) -> Self {
        Self {
            listener: OneShotCell::new(TcpListener::with_buffers(
                stack,
                local_endpoint,
                rx_buffers,
                tx_buffers,
            )),
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }

    /// Use a prepared [`AimxConfig`] for limits and security policy.
    pub fn with_config(mut self, config: AimxConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the security policy.
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.config = self.config.security_policy(policy);
        self
    }

    /// Maximum concurrently served connections.
    ///
    /// Effective concurrency is `min(N, max)`, because every active worker owns
    /// exactly one listening or connected socket.
    pub fn max_connections(mut self, max: usize) -> Self {
        self.config = self.config.max_connections(max);
        self
    }

    /// Maximum live subscriptions per connection.
    pub fn max_subs_per_connection(mut self, max: usize) -> Self {
        self.config = self.config.max_subs_per_connection(max);
        self
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

impl<const N: usize> ConnectorBuilder for TcpServer<N> {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        let listener = self.listener.take_required();
        let config = self.config.clone();
        Box::pin(SendFutureWrapper(async move {
            let listener = listener?;
            crate::apply_writable(db, &config);
            let session_config = SessionConfig {
                limits: SessionLimits {
                    max_connections: config.max_connections,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                acks_subscribe: false,
            };
            let dispatch: Arc<dyn Dispatch> = Arc::new(
                aimdb_core::session::aimx::AimxDispatch::new(Arc::new(db.clone()), config),
            );
            let max_workers = session_config.limits.max_connections;
            Ok(listener.into_server_futures(
                Arc::new(AimxCodec),
                dispatch,
                session_config,
                max_workers,
            ))
        }))
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}
