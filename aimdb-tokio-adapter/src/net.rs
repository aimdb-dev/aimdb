//! Tokio implementations of core's runtime-neutral I/O traits (design 052
//! §3.2), the std dual of `aimdb-embassy-adapter::net`.
//!
//! Every future here is a plain `async fn`: the compiler proves it `Send`, so
//! the `+ Send` the traits declare on their return types costs nothing and no
//! `unsafe` appears anywhere in this module. That asymmetry with the Embassy
//! side — which force-`Send`s a `!Send` inner future through a transparent
//! newtype — is the whole point of putting the bound on the trait rather than
//! at the use site.

use std::net::SocketAddr;

use aimdb_core::session::{
    ByteStream, Datagram, DatagramBinder, Delay, PeerInfo, StreamDialer, StreamListener,
    TransportError, TransportResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Tokio network entry point — the std counterpart of `EmbassyNet`.
pub struct TokioNet;

impl TokioNet {
    /// A TCP dialer. Name resolution is the adapter's job, so the connector
    /// hands over a host string and never touches `std::net`.
    pub fn tcp() -> TokioTcpDialer {
        TokioTcpDialer
    }

    /// Bind a TCP listener. Binding here (rather than in `build`) is what the
    /// `?` in design 052 §8's `TcpServer::new(TokioNet::listen(addr)?)` is.
    pub async fn listen(addr: &str) -> TransportResult<TokioTcpListener> {
        TcpListener::bind(addr)
            .await
            .map(TokioTcpListener)
            .map_err(|_| TransportError::Io)
    }

    /// A UDP binder for KNX/IP and SNTP.
    pub fn udp(bind_addr: impl Into<String>) -> TokioUdpBinder {
        TokioUdpBinder {
            bind_addr: bind_addr.into(),
        }
    }

    /// The clock, as a non-boxing [`Delay`].
    pub fn delay() -> TokioDelay {
        TokioDelay
    }
}

// ===========================================================================
// Streams.
// ===========================================================================

/// One Tokio TCP connection as a [`ByteStream`].
pub struct TokioByteStream<S>(pub S);

impl<S> ByteStream for TokioByteStream<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        self.0.read(buf).await.map_err(|_| TransportError::Io)
    }

    async fn write_all(&mut self, buf: &[u8]) -> TransportResult<()> {
        self.0
            .write_all(buf)
            .await
            .map_err(|_| TransportError::Closed)
    }

    async fn flush(&mut self) -> TransportResult<()> {
        self.0.flush().await.map_err(|_| TransportError::Closed)
    }
}

/// Dials TCP connections.
pub struct TokioTcpDialer;

impl StreamDialer for TokioTcpDialer {
    type Stream = TokioByteStream<TcpStream>;

    async fn connect(&self, host: &str, port: u16) -> TransportResult<Self::Stream> {
        TcpStream::connect((host, port))
            .await
            .map(TokioByteStream)
            .map_err(|_| TransportError::Io)
    }
}

/// Accepts TCP connections.
pub struct TokioTcpListener(TcpListener);

impl TokioTcpListener {
    /// The address actually bound (useful when binding to port 0).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.local_addr().ok()
    }
}

impl StreamListener for TokioTcpListener {
    type Stream = TokioByteStream<TcpStream>;

    async fn accept(&mut self) -> TransportResult<(Self::Stream, PeerInfo)> {
        let (stream, addr) = self.0.accept().await.map_err(|_| TransportError::Io)?;
        // `PeerInfo` is `#[non_exhaustive]`; build it by mutation.
        let mut peer = PeerInfo::default();
        peer.peer_addr = Some(addr.to_string());
        Ok((TokioByteStream(stream), peer))
    }
}

// ===========================================================================
// Datagrams.
// ===========================================================================

/// One bound Tokio UDP socket as a [`Datagram`].
pub struct TokioDatagram {
    socket: UdpSocket,
    local: Option<SocketAddr>,
}

impl Datagram for TokioDatagram {
    async fn send_to(&mut self, buf: &[u8], to: SocketAddr) -> TransportResult<()> {
        self.socket
            .send_to(buf, to)
            .await
            .map(|_| ())
            .map_err(|_| TransportError::Io)
    }

    async fn recv_from(&mut self, buf: &mut [u8]) -> TransportResult<(usize, SocketAddr)> {
        self.socket
            .recv_from(buf)
            .await
            .map_err(|_| TransportError::Io)
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        self.local
    }
}

/// Binds [`TokioDatagram`]s, one per reconnect cycle.
pub struct TokioUdpBinder {
    bind_addr: String,
}

impl DatagramBinder for TokioUdpBinder {
    type Socket = TokioDatagram;

    async fn bind(&self, port: u16) -> TransportResult<Self::Socket> {
        // `port` overrides the binder's default when non-zero, matching the
        // Embassy binder's contract (0 = any).
        let addr = if port == 0 {
            self.bind_addr.clone()
        } else {
            format!("0.0.0.0:{port}")
        };
        let socket = UdpSocket::bind(&addr)
            .await
            .map_err(|_| TransportError::Io)?;
        let local = socket.local_addr().ok();
        Ok(TokioDatagram { socket, local })
    }
}

// ===========================================================================
// Clock.
// ===========================================================================

/// [`Delay`] over `tokio::time::sleep`. Generic, so nothing is boxed —
/// contrast `RuntimeOps::sleep`, which is `dyn` and allocates per call.
#[derive(Clone, Copy, Default)]
pub struct TokioDelay;

impl Delay for TokioDelay {
    fn sleep(&self, d: std::time::Duration) -> impl std::future::Future<Output = ()> + Send {
        tokio::time::sleep(d)
    }
}
