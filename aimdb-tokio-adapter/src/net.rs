//! Tokio implementations of core's runtime-neutral I/O traits — the std dual of
//! the Embassy adapter's `net` module.
//!
//! Every future here is a plain `async fn`: the compiler proves it `Send`, so
//! the `+ Send` the traits declare on their return types costs nothing and this
//! module contains no `unsafe`. The Embassy side, whose socket futures are
//! `!Send`, wraps them instead — that asymmetry is why the bound sits on the
//! trait rather than at each use site.

use std::net::{IpAddr, SocketAddr};

use aimdb_core::session::{
    ByteStream, Datagram, DatagramBinder, Delay, PeerInfo, StreamDialer, StreamListener,
    TransportError, TransportResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Entry point for the Tokio transports.
pub struct TokioNet;

impl TokioNet {
    /// A TCP dialer. Name resolution happens here, so a connector hands over a
    /// host string and never touches `std::net`.
    pub fn tcp() -> TokioTcpDialer {
        TokioTcpDialer
    }

    /// Bind a TCP listener on `addr` (`"host:port"`).
    pub async fn listen(addr: &str) -> TransportResult<TokioTcpListener> {
        TcpListener::bind(addr)
            .await
            .map(TokioTcpListener)
            .map_err(|_| TransportError::Io)
    }

    /// A UDP binder on `local_ip`, which sockets are bound to as
    /// `local_ip:port`. Pass [`Ipv4Addr::UNSPECIFIED`](std::net::Ipv4Addr) for
    /// any interface.
    pub fn udp(local_ip: impl Into<IpAddr>) -> TokioUdpBinder {
        TokioUdpBinder {
            local_ip: local_ip.into(),
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

/// Any Tokio async byte source as a [`ByteStream`] — a `TcpStream`, a
/// `tokio::io::duplex` pipe, a serial port.
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
    /// The address actually bound — the way to learn the port after binding
    /// one of the ephemeral `:0` forms.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.local_addr().ok()
    }
}

impl StreamListener for TokioTcpListener {
    type Stream = TokioByteStream<TcpStream>;

    async fn accept(&mut self) -> TransportResult<(Self::Stream, PeerInfo)> {
        let (stream, addr) = self.0.accept().await.map_err(|_| TransportError::Io)?;
        // `PeerInfo` is `#[non_exhaustive]`, so it is built by mutation.
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
    local_ip: IpAddr,
}

impl DatagramBinder for TokioUdpBinder {
    type Socket = TokioDatagram;

    async fn bind(&self, port: u16) -> TransportResult<Self::Socket> {
        let socket = UdpSocket::bind(SocketAddr::new(self.local_ip, port))
            .await
            .map_err(|_| TransportError::Io)?;
        let local = socket.local_addr().ok();
        Ok(TokioDatagram { socket, local })
    }
}

// ===========================================================================
// Clock.
// ===========================================================================

/// [`Delay`] over `tokio::time::sleep`, returning the timer itself rather than
/// a boxed future.
#[derive(Clone, Copy, Default)]
pub struct TokioDelay;

impl Delay for TokioDelay {
    fn sleep(&self, d: std::time::Duration) -> impl std::future::Future<Output = ()> + Send {
        tokio::time::sleep(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::session::{Dialer, Framer, FramingDialer, FramingListener, Listener};
    use std::net::Ipv4Addr;

    /// Length-prefixed framer, enough to drive a `FramedConnection`.
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
            if self.buf.len() < len + 1 {
                return None;
            }
            let frame = self.buf[1..len + 1].to_vec();
            self.buf.drain(..len + 1);
            Some(Ok(frame))
        }
    }

    #[tokio::test]
    async fn tcp_round_trips_between_dialer_and_listener() {
        let mut listener = TokioNet::listen("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (mut stream, peer) = listener.accept().await.unwrap();
            assert!(
                peer.peer_addr.is_some(),
                "accept must carry the peer address"
            );
            let mut buf = [0u8; 16];
            let n = stream.read(&mut buf).await.unwrap();
            stream.write_all(&buf[..n]).await.unwrap();
            stream.flush().await.unwrap();
        });

        let mut client = TokioNet::tcp().connect("127.0.0.1", port).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 16];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_closed_peer_reads_as_eof() {
        let mut listener = TokioNet::listen("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });

        let mut client = TokioNet::tcp().connect("127.0.0.1", port).await.unwrap();
        server.await.unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(client.read(&mut buf).await.unwrap(), 0, "EOF is Ok(0)");
    }

    /// The transports drive core's `Dialer`/`Listener` unchanged, which is the
    /// point of the adapter owning sockets.
    #[tokio::test]
    async fn the_transports_drive_a_framed_connection() {
        let listener = TokioNet::listen("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut listener: FramingListener<_, _, 256, 256> =
            FramingListener::new(listener, LenFramer::default);

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let frame = conn.recv().await.unwrap().unwrap();
            conn.send(&frame).await.unwrap();
        });

        let dialer: FramingDialer<_, _, 256, 256> =
            FramingDialer::new(TokioNet::tcp(), LenFramer::default, "127.0.0.1", port);
        let mut conn = dialer.connect().await.unwrap();
        conn.send(b"hello").await.unwrap();
        assert_eq!(conn.recv().await.unwrap(), Some(b"hello".to_vec()));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn udp_round_trips_and_reports_its_bound_address() {
        let binder = TokioNet::udp(Ipv4Addr::LOCALHOST);
        let mut a = binder.bind(0).await.unwrap();
        let mut b = binder.bind(0).await.unwrap();

        let a_addr = a.local_addr().expect("a bound address must be reported");
        let b_addr = b.local_addr().expect("a bound address must be reported");
        assert_ne!(
            a_addr.port(),
            0,
            "an ephemeral bind resolves to a real port"
        );

        a.send_to(b"knx", b_addr).await.unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"knx");
        assert_eq!(from, a_addr, "the source address must be the sender's");
    }

    /// Rebinding is what the KNX socket reset needs: a fresh socket each cycle,
    /// on the same binder.
    #[tokio::test]
    async fn a_binder_can_rebind_after_its_socket_is_dropped() {
        let binder = TokioNet::udp(Ipv4Addr::LOCALHOST);
        let first = binder.bind(0).await.unwrap();
        let port = first.local_addr().unwrap().port();
        drop(first);

        let second = binder.bind(port).await.unwrap();
        assert_eq!(second.local_addr().unwrap().port(), port);
    }

    #[tokio::test]
    async fn delay_sleeps_without_boxing() {
        let start = std::time::Instant::now();
        TokioNet::delay()
            .sleep(std::time::Duration::from_millis(20))
            .await;
        assert!(start.elapsed() >= std::time::Duration::from_millis(15));
    }
}
