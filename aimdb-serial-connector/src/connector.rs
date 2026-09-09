//! Runtime-neutral serial client and server sugar.
//!
//! Both take a byte stream from an adapter — `EmbassyUart` on the MCU,
//! `TokioByteStream` over a `SerialStream` on the host — and this crate
//! contributes only the COBS [`CobsFramer`].
//!
//! A UART is point-to-point, so the stream is moved in and served once; there
//! is no accept loop. `SerialPortDialer` (feature `std`) is the one exception,
//! because a host can reopen a device by path.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::log_info;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    BoxFut, ByteStream, ClientConfig, Connection, Dialer, Dispatch, FramedConnection, Listener,
    OneShot, SessionClientConnector, SessionConfig, SessionLimits, SessionServerConnector,
    TransportError, TransportResult,
};
use aimdb_core::{AimDb, DbError, DbResult};

use crate::framing::{CobsFramer, READ_CHUNK, WRITE_CHUNK};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// A COBS-framed connection over any adapter byte stream.
pub type SerialFramed<S> = FramedConnection<S, CobsFramer, READ_CHUNK, WRITE_CHUNK>;

/// Frame an adapter's byte stream with COBS.
pub fn framed<S: ByteStream>(stream: S) -> SerialFramed<S> {
    FramedConnection::new(stream, CobsFramer::new())
}

/// Hands out one pre-built connection, then refuses.
///
/// A UART is point-to-point: there is nothing to redial, so the second attempt
/// is an error rather than a silent reconnect loop.
pub struct OneShotDialer<C> {
    conn: OneShot<C>,
}

impl<C> OneShotDialer<C> {
    /// Hold `conn` for the first [`connect`](Dialer::connect).
    pub fn new(conn: C) -> Self {
        Self {
            conn: OneShot::new(conn),
        }
    }
}

impl<C: Connection + 'static> Dialer for OneShotDialer<C> {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            self.conn
                .take()
                .map(|c| Box::new(c) as Box<dyn Connection>)
                .ok_or(TransportError::Closed)
        })
    }
}

/// Hands out one pre-built connection, then parks forever.
///
/// The [`Listener`] dual of [`OneShotDialer`]: `serve` loops on `accept`, and a
/// point-to-point link has no second peer to accept, so parking is the correct
/// end state rather than an error the loop would spin on.
pub struct OneShotListener<C> {
    conn: OneShot<C>,
}

impl<C> OneShotListener<C> {
    /// Hold `conn` for the first [`accept`](Listener::accept).
    pub fn new(conn: C) -> Self {
        Self {
            conn: OneShot::new(conn),
        }
    }
}

impl<C: Connection + 'static> Listener for OneShotListener<C> {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            match self.conn.take() {
                Some(c) => Ok(Box::new(c) as Box<dyn Connection>),
                None => core::future::pending().await,
            }
        })
    }
}

/// Opens a serial device by path on each [`connect`](Dialer::connect).
///
/// The one redialable serial dialer: a host can reopen a device, so
/// `run_client` reconnects after a drop. Cheap to clone (path plus baud).
/// `tokio-serial` stays here rather than in the adapter — opening a tty is not
/// a runtime concern.
#[cfg(feature = "std")]
#[derive(Clone)]
pub struct SerialPortDialer {
    path: String,
    baud: u32,
}

#[cfg(feature = "std")]
impl SerialPortDialer {
    /// Dial the serial device at `path` (e.g. `/dev/ttyUSB0`) at `baud`.
    pub fn new(path: impl Into<String>, baud: u32) -> Self {
        Self {
            path: path.into(),
            baud,
        }
    }
}

#[cfg(feature = "std")]
impl Dialer for SerialPortDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            use tokio_serial::SerialPortBuilderExt;
            let stream = tokio_serial::new(&self.path, self.baud)
                .open_native_async()
                .map_err(|_| TransportError::Io)?;
            // Discard bytes a previous session left in the OS input buffer;
            // otherwise the first frame is a stale leftover that fails to decode
            // and desyncs the stream until the next COBS sentinel.
            use tokio_serial::SerialPort;
            let _ = stream.clear(tokio_serial::ClearBuffer::Input);
            Ok(
                Box::new(framed(aimdb_tokio_adapter::net::TokioByteStream(stream)))
                    as Box<dyn Connection>,
            )
        })
    }
}

/// Constructs a serial session client connector.
pub struct SerialClient;

impl SerialClient {
    /// Mirror records to and from the AimX peer on `stream`, served once.
    ///
    /// Reconnect is **disabled** (unlike `ClientConfig::default`): the stream is
    /// moved in and cannot be re-acquired. `run_client` would stop anyway —
    /// it treats a dialer's [`TransportError::Closed`] as terminal — but
    /// saying so here keeps the intent local rather than resting on that
    /// two-crate handshake, and skips a pointless backoff and "dial failed"
    /// warning on the way out. A caller whose stream really can be redialed
    /// opts back in with `.with_config(...)`; on a host, prefer
    /// [`over_port`](Self::over_port), which reopens the device for real.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<S>(stream: S) -> SessionClientConnector<OneShotDialer<SerialFramed<S>>, AimxCodec>
    where
        S: ByteStream + Send + 'static,
    {
        SessionClientConnector::new(OneShotDialer::new(framed(stream)), AimxCodec)
            .scheme(DEFAULT_SCHEME)
            .with_config(ClientConfig {
                reconnect: false,
                ..ClientConfig::default()
            })
    }

    /// Mirror records over a serial device this process opens by path,
    /// reconnecting after a drop.
    ///
    /// Keeps `ClientConfig`'s default `reconnect: true` — unlike
    /// [`new`](Self::new), this dialer can genuinely reopen the device.
    #[cfg(feature = "std")]
    pub fn over_port(
        path: impl Into<String>,
        baud: u32,
    ) -> SessionClientConnector<SerialPortDialer, AimxCodec> {
        SessionClientConnector::new(SerialPortDialer::new(path, baud), AimxCodec)
            .scheme(DEFAULT_SCHEME)
    }
}

/// Serves AimX over a moved-in serial stream.
pub struct SerialServer<S> {
    stream: OneShot<S>,
    config: AimxConfig,
    scheme: String,
}

impl<S> SerialServer<S> {
    /// Serve AimX over `stream`.
    pub fn new(stream: S) -> Self {
        Self {
            stream: OneShot::new(stream),
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

impl<S> ConnectorBuilder for SerialServer<S>
where
    S: ByteStream + Send + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        let config = self.config.clone();
        let scheme = self.scheme.clone();
        Box::pin(async move {
            // Taken on first poll, not at call time: a `build()` future dropped
            // before it is polled must leave the stream where it was.
            let stream = self
                .stream
                .take()
                .ok_or_else(|| DbError::InvalidOperation {
                    operation: "SerialServer::build".to_string(),
                    reason: "the moved-in stream was already taken; build() ran twice".to_string(),
                })?;
            log_info!("Initializing AimX serial server on scheme '{}'", scheme);
            let session_config = SessionConfig {
                limits: SessionLimits {
                    // A UART carries a single peer.
                    max_connections: 1,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                // AimX's subscribe ack stays implicit (events flow); no ack frame.
                acks_subscribe: false,
            };
            let listener = OneShot::new(OneShotListener::new(framed(stream)));
            let dispatch_config = config;
            let connector = SessionServerConnector::new(
                move || {
                    listener.take().ok_or_else(|| DbError::InvalidOperation {
                        operation: "SerialServer::build".to_string(),
                        reason: "the moved-in stream was already taken".to_string(),
                    })
                },
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
