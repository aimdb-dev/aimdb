//! tokio serial transport (feature `tokio-runtime`) — a [`Connection`] over an
//! async byte stream with COBS framing in the transport, plus
//! [`SerialClient`]/[`SerialServer`] sugar over the generic core connectors.
//!
//! The connection is generic over `AsyncRead + AsyncWrite` so it backs a real
//! `tokio_serial::SerialStream` in production and a `tokio::io::duplex()` pipe in
//! tests — no hardware needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    BoxFut, Connection, Dialer, Dispatch, Listener, PeerInfo, SessionClientConnector,
    SessionConfig, SessionLimits, SessionServerConnector, TransportError, TransportResult,
};
use aimdb_core::{AimDb, DbError, DbResult, RuntimeAdapter};

use crate::framing::{encode_frame, FrameAccumulator};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// How many bytes a single serial `read()` pulls before re-checking for a frame.
const READ_CHUNK: usize = 256;

// ===========================================================================
// Connection
// ===========================================================================

/// A framed bidirectional pipe over an async serial byte stream. Framing lives in
/// the transport: [`recv`](Connection::recv) returns one COBS frame (sentinel
/// stripped); [`send`](Connection::send) COBS-encodes and appends the sentinel.
pub struct TokioSerialConnection<S> {
    stream: S,
    acc: FrameAccumulator,
    peer: PeerInfo,
}

impl<S> TokioSerialConnection<S> {
    /// Wrap an already-open async byte stream (a `SerialStream`, or a duplex pipe
    /// in tests).
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            acc: FrameAccumulator::new(),
            peer: PeerInfo::default(),
        }
    }
}

impl<S> Connection for TokioSerialConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            loop {
                // COBS is self-synchronizing: a chunk that fails to decode is line
                // noise or a mid-stream join, not a fatal transport error. The
                // accumulator has already consumed it, so skip it and resync on the
                // next sentinel rather than tearing down the session.
                match self.acc.next_frame() {
                    Some(Ok(frame)) => return Ok(Some(frame)),
                    Some(Err(_)) => continue,
                    None => {}
                }
                let mut chunk = [0u8; READ_CHUNK];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return Ok(None), // EOF — peer closed
                    Ok(n) => self.acc.push_bytes(&chunk[..n]),
                    Err(_) => return Err(TransportError::Io),
                }
            }
        })
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async move {
            let mut out = Vec::new();
            encode_frame(frame, &mut out);
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

// ===========================================================================
// Dialer / Listener
// ===========================================================================

/// The initiating (client) side: opens the serial port on each
/// [`connect`](Dialer::connect). Cheap to clone (path + baud), so `run_client`
/// can redial and the generic `SessionClientConnector` can hold it.
#[derive(Clone)]
pub struct SerialDialer {
    path: String,
    baud: u32,
}

impl SerialDialer {
    /// Dial the serial device at `path` (e.g. `/dev/ttyUSB0`) at `baud`.
    pub fn new(path: impl Into<String>, baud: u32) -> Self {
        Self {
            path: path.into(),
            baud,
        }
    }
}

impl Dialer for SerialDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let stream = tokio_serial::new(&self.path, self.baud)
                .open_native_async()
                .map_err(|_| TransportError::Io)?;
            // Discard any bytes left in the OS input buffer by a previous session
            // (e.g. a half-read reply from a killed client). Otherwise the first
            // frame is a stale leftover that fails to decode and desyncs the stream
            // until the next COBS sentinel — a transient `Internal` on the first
            // call or two.
            use tokio_serial::SerialPort;
            let _ = stream.clear(tokio_serial::ClearBuffer::Input);
            Ok(Box::new(TokioSerialConnection::new(stream)) as Box<dyn Connection>)
        })
    }
}

/// The accepting (server) side. Serial is point-to-point, so this is a one-shot
/// listener: the first [`accept`](Listener::accept) hands out the (already-open)
/// port; later calls park forever (there is only ever one peer on a UART).
pub struct SerialListener {
    stream: Option<SerialStream>,
}

impl SerialListener {
    /// Wrap an already-open serial port.
    pub fn new(stream: SerialStream) -> Self {
        Self {
            stream: Some(stream),
        }
    }
}

impl Listener for SerialListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            match self.stream.take() {
                Some(s) => Ok(Box::new(TokioSerialConnection::new(s)) as Box<dyn Connection>),
                // Point-to-point: no second peer ever arrives.
                None => core::future::pending().await,
            }
        })
    }
}

// ===========================================================================
// Client sugar
// ===========================================================================

/// Constructs a [`SessionClientConnector`] that dials an AimX peer over a serial
/// port. `SerialClient::new(path, baud)` is sugar; chain `.scheme(...)` /
/// `.with_config(...)` on the returned connector.
pub struct SerialClient;

impl SerialClient {
    /// Mirror records to/from the AimX peer reachable at serial `path` (scheme
    /// defaults to [`DEFAULT_SCHEME`]).
    // Sugar constructor: intentionally returns the generic connector, not `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        path: impl Into<String>,
        baud: u32,
    ) -> SessionClientConnector<SerialDialer, AimxCodec> {
        SessionClientConnector::new(SerialDialer::new(path, baud), AimxCodec).scheme(DEFAULT_SCHEME)
    }
}

// ===========================================================================
// Server sugar
// ===========================================================================

/// Accepts an AimX connection over a serial port and serves the full AimX
/// toolset. Register it via `with_connector` to let a host (or another board)
/// query this db over a UART.
pub struct SerialServer {
    path: String,
    baud: u32,
    config: AimxConfig,
    scheme: String,
}

impl SerialServer {
    /// Serve AimX over the serial device at `path` (e.g. `/dev/ttyUSB0`) at
    /// `baud`, with the default read-only policy / limits.
    pub fn new(path: impl Into<String>, baud: u32) -> Self {
        Self {
            path: path.into(),
            baud,
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }

    /// Use a prepared [`AimxConfig`] for the security policy / limits (the
    /// `socket_path` / `socket_permissions` fields are unused over serial).
    pub fn with_config(mut self, config: AimxConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the security policy (read-only vs. per-record-writable).
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.config = self.config.security_policy(policy);
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

impl<R> ConnectorBuilder<R> for SerialServer
where
    R: RuntimeAdapter + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb<R>) -> BuildFuture<'a> {
        let path = self.path.clone();
        let baud = self.baud;
        let config = self.config.clone();
        let scheme = self.scheme.clone();
        Box::pin(async move {
            let session_config = SessionConfig {
                limits: SessionLimits {
                    // A UART carries a single peer; cap connections at 1.
                    max_connections: 1,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                // AimX's subscribe ack stays implicit (events flow); no ack frame.
                acks_subscribe: false,
            };
            let dispatch_config = config;
            // Reuse the generic spine: open the port (errors surface synchronously)
            // + AimX dispatch over the AimX codec.
            let connector = SessionServerConnector::new(
                move || open_serial_listener(&path, baud),
                AimxCodec,
                move |db: &AimDb<R>| -> Arc<dyn Dispatch> {
                    // Apply the security policy's writable marking so `record.list`
                    // reports the `writable` flag (the dispatch also enforces it).
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

// ===========================================================================
// Helpers
// ===========================================================================

/// Open the serial port synchronously so an open error surfaces from `build`.
fn open_serial_listener(path: &str, baud: u32) -> DbResult<SerialListener> {
    #[cfg(feature = "tracing")]
    tracing::info!(
        "Initializing AimX serial server on {} @ {} baud",
        path,
        baud
    );

    let stream = tokio_serial::new(path, baud)
        .open_native_async()
        .map_err(|e| DbError::IoWithContext {
            context: format!("Failed to open serial port {} @ {} baud", path, baud),
            source: std::io::Error::other(e),
        })?;
    Ok(SerialListener::new(stream))
}
