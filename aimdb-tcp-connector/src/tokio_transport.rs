//! Tokio TCP transport (feature `tokio-runtime`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener as TokioTcpListener, TcpStream};

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    BoxFut, Connection, Dialer, Dispatch, Listener, PeerInfo, SessionClientConnector,
    SessionConfig, SessionLimits, SessionServerConnector, TransportError, TransportResult,
};
use aimdb_core::{AimDb, DbError, DbResult};

use crate::framing::{encode_frame, FrameAccumulator, DEFAULT_MAX_FRAME, HEADER_LEN};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

const READ_CHUNK: usize = 1024;

/// A framed TCP connection.
pub struct TcpConnection<S> {
    stream: S,
    acc: FrameAccumulator,
    peer: PeerInfo,
}

impl<S> TcpConnection<S> {
    /// Wrap an already-connected async stream with default frame limits.
    pub fn new(stream: S) -> Self {
        Self::with_max_frame(stream, DEFAULT_MAX_FRAME)
    }

    /// Wrap an already-connected async stream with a caller-provided frame cap.
    pub fn with_max_frame(stream: S, max_frame: usize) -> Self {
        Self {
            stream,
            acc: FrameAccumulator::with_max_frame(max_frame),
            peer: PeerInfo::default(),
        }
    }

    fn with_peer(mut self, peer: PeerInfo) -> Self {
        self.peer = peer;
        self
    }
}

impl<S> Connection for TcpConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            loop {
                match self.acc.next_frame() {
                    Some(Ok(frame)) => return Ok(Some(frame)),
                    Some(Err(_)) => return Err(TransportError::Io),
                    None => {}
                }

                let mut chunk = [0u8; READ_CHUNK];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return Ok(None),
                    Ok(n) => self.acc.push_bytes(&chunk[..n]),
                    Err(_) => return Err(TransportError::Io),
                }
            }
        })
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async move {
            let capacity = HEADER_LEN
                .checked_add(frame.len())
                .ok_or(TransportError::Io)?;
            let mut out = Vec::with_capacity(capacity);
            encode_frame(frame, &mut out).map_err(|_| TransportError::Io)?;
            self.stream
                .write_all(&out)
                .await
                .map_err(|_| TransportError::Closed)?;
            self.stream
                .flush()
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// The initiating side: dials a TCP endpoint on each connect.
#[derive(Clone)]
pub struct TcpDialer {
    endpoint: String,
    max_frame: usize,
}

impl TcpDialer {
    /// Dial `endpoint`, for example `127.0.0.1:7001`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            max_frame: DEFAULT_MAX_FRAME,
        }
    }

    /// Set maximum frame payload size.
    pub fn max_frame(mut self, max_frame: usize) -> Self {
        self.max_frame = max_frame;
        self
    }
}

impl Dialer for TcpDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let stream = TcpStream::connect(&self.endpoint)
                .await
                .map_err(|_| TransportError::Io)?;
            let peer_addr = stream.peer_addr().ok().map(|a| a.to_string());
            let mut peer = PeerInfo::default();
            peer.peer_addr = peer_addr;
            Ok(
                Box::new(TcpConnection::with_max_frame(stream, self.max_frame).with_peer(peer))
                    as Box<dyn Connection>,
            )
        })
    }
}

/// The accepting side.
pub struct TcpListener {
    inner: TokioTcpListener,
    max_frame: usize,
}

impl TcpListener {
    /// Wrap an already-bound listener.
    pub fn new(inner: TokioTcpListener) -> Self {
        Self {
            inner,
            max_frame: DEFAULT_MAX_FRAME,
        }
    }

    /// Set maximum frame payload size.
    pub fn max_frame(mut self, max_frame: usize) -> Self {
        self.max_frame = max_frame;
        self
    }
}

impl Listener for TcpListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let (stream, addr) = self.inner.accept().await.map_err(|_| TransportError::Io)?;
            let mut peer = PeerInfo::default();
            peer.peer_addr = Some(addr.to_string());
            Ok(
                Box::new(TcpConnection::with_max_frame(stream, self.max_frame).with_peer(peer))
                    as Box<dyn Connection>,
            )
        })
    }
}

/// Constructs a TCP session client connector.
pub struct TcpClient;

impl TcpClient {
    /// Mirror records to/from an AimX peer over TCP.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(endpoint: impl Into<String>) -> SessionClientConnector<TcpDialer, AimxCodec> {
        SessionClientConnector::new(TcpDialer::new(endpoint), AimxCodec).scheme(DEFAULT_SCHEME)
    }
}

/// Accepts AimX connections over TCP.
pub struct TcpServer {
    bind_addr: String,
    config: AimxConfig,
    scheme: String,
    max_frame: usize,
}

impl TcpServer {
    /// Serve AimX on `bind_addr`.
    ///
    /// Prefer loopback addresses such as `127.0.0.1:7001` unless the deployment
    /// provides its own network-layer protection.
    pub fn new(bind_addr: impl Into<String>) -> Self {
        Self {
            bind_addr: bind_addr.into(),
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
            max_frame: DEFAULT_MAX_FRAME,
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
    pub fn max_connections(mut self, max: usize) -> Self {
        self.config = self.config.max_connections(max);
        self
    }

    /// Maximum live subscriptions per connection.
    pub fn max_subs_per_connection(mut self, max: usize) -> Self {
        self.config = self.config.max_subs_per_connection(max);
        self
    }

    /// Maximum TCP frame payload size.
    pub fn max_frame(mut self, max_frame: usize) -> Self {
        self.max_frame = max_frame;
        self
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

impl ConnectorBuilder for TcpServer {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        let bind_addr = self.bind_addr.clone();
        let config = self.config.clone();
        let scheme = self.scheme.clone();
        let max_frame = self.max_frame;
        Box::pin(async move {
            let session_config = SessionConfig {
                limits: SessionLimits {
                    max_connections: config.max_connections,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                acks_subscribe: false,
            };
            let bind_config = bind_addr.clone();
            let dispatch_config = config;
            let connector = SessionServerConnector::new(
                move || bind_tcp_listener(&bind_config, max_frame),
                AimxCodec,
                move |db: &AimDb| -> Arc<dyn Dispatch> {
                    crate::apply_writable(db, &dispatch_config);
                    Arc::new(AimxDispatch::new(
                        Arc::new(db.clone()),
                        dispatch_config.clone(),
                    ))
                },
                session_config,
            )
            .scheme(scheme);
            connector.build(db).await
        })
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

fn bind_tcp_listener(addr: &str, max_frame: usize) -> DbResult<TcpListener> {
    let listener = std::net::TcpListener::bind(addr).map_err(|e| DbError::IoWithContext {
        context: "Failed to bind TCP listener".to_string(),
        source: e,
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to set TCP listener nonblocking".to_string(),
            source: e,
        })?;
    let listener = TokioTcpListener::from_std(listener).map_err(|e| DbError::IoWithContext {
        context: "Failed to create Tokio TCP listener".to_string(),
        source: e,
    })?;
    Ok(TcpListener::new(listener).max_frame(max_frame))
}
