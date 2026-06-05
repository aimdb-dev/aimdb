//! Embassy serial transport (feature `embassy-runtime`, `no_std + alloc`) — a
//! [`Connection`] over an [`embedded_io_async`] UART with COBS framing, plus
//! [`SerialClient`]/[`SerialServer`] sugar.
//!
//! Generic over the `embedded-io-async` `Read`/`Write` halves (the common Embassy
//! HAL shape, e.g. `Uart::split()`), so it works with any chip's async UART.
//!
//! # Why this half hand-rolls `ConnectorBuilder`
//!
//! The generic [`SessionClientConnector`](aimdb_core::session::SessionClientConnector)
//! / [`SessionServerConnector`](aimdb_core::session::SessionServerConnector) demand
//! `Clone + Send + Sync` on the dialer / `Send + Sync` on the listener+dispatch
//! factories — bounds a moved-in UART peripheral can't meet. The underlying
//! engines need only the bare traits ([`run_client`]`<D: Dialer>`, [`serve`]`<L:
//! Listener>`), so we call them directly and force-`Send` the (single-core,
//! cooperative) Embassy futures with `aimdb-embassy-adapter`'s
//! [`SendFutureWrapper`] — the same pattern the MQTT/KNX Embassy connectors use.
//!
//! The `unsafe impl Send`/`Sync` on the transport + builder types rest on the same
//! invariant `SendFutureWrapper` documents: an Embassy executor runs cooperatively
//! on a single core with no preemption or thread migration, so these values are
//! never actually accessed from another thread.

use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use embedded_io_async::{Read, Write};

use aimdb_embassy_adapter::SendFutureWrapper;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    pump_client, run_client, serve, BoxFut, ClientConfig, Connection, Dialer, Dispatch, Listener,
    PeerInfo, SessionConfig, SessionLimits, TransportError, TransportResult,
};
use aimdb_core::{AimDb, DbError, DbResult, RuntimeAdapter};
use aimdb_executor::TimeOps;

use crate::framing::{encode_frame, FrameAccumulator};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// How many bytes a single UART `read()` pulls before re-checking for a frame.
const READ_CHUNK: usize = 64;

/// Max bytes per `write()` call. Some HAL `BufferedUart::write` is atomic-or-error
/// (e.g. `embassy-stm32` returns `BufferTooLong` for a single write larger than its
/// TX ring), so a frame bigger than the buffer must be split. Chunking at this size
/// sends a frame of any length as long as the TX buffer is at least this big.
const WRITE_CHUNK: usize = 64;

// ===========================================================================
// Connection
// ===========================================================================

/// A framed bidirectional pipe over an `embedded-io-async` UART. Framing lives in
/// the transport: [`recv`](Connection::recv) returns one COBS frame (sentinel
/// stripped); [`send`](Connection::send) COBS-encodes and appends the sentinel.
pub struct EmbassySerialConnection<Rd, Wr> {
    rx: Rd,
    tx: Wr,
    acc: FrameAccumulator,
    peer: PeerInfo,
}

// SAFETY: Embassy executors run cooperatively on a single core with no preemption
// or thread migration, so the wrapped UART halves are never accessed across
// threads. Only construct this where it is driven by an Embassy executor. Same
// invariant as `aimdb_embassy_adapter::SendFutureWrapper`.
unsafe impl<Rd, Wr> Send for EmbassySerialConnection<Rd, Wr> {}

impl<Rd, Wr> EmbassySerialConnection<Rd, Wr> {
    /// Wrap the split read/write halves of an async UART.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self {
            rx,
            tx,
            acc: FrameAccumulator::new(),
            peer: PeerInfo::default(),
        }
    }
}

impl<Rd, Wr> Connection for EmbassySerialConnection<Rd, Wr>
where
    Rd: Read,
    Wr: Write,
{
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        // `SendFutureWrapper` force-`Send`s the (single-core) UART read future to
        // satisfy the `Send` `BoxFut` return type.
        Box::pin(SendFutureWrapper(async move {
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
                match self.rx.read(&mut chunk).await {
                    Ok(0) => return Ok(None), // EOF — peer closed
                    Ok(n) => self.acc.push_bytes(&chunk[..n]),
                    Err(_) => return Err(TransportError::Io),
                }
            }
        }))
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(SendFutureWrapper(async move {
            let mut out = Vec::new();
            encode_frame(frame, &mut out);
            // Write in ring-sized chunks: a HAL `BufferedUart` rejects a single
            // write larger than its TX buffer, so a frame bigger than the buffer
            // (e.g. a `record.list` reply) must be split.
            for chunk in out.chunks(WRITE_CHUNK) {
                self.tx
                    .write_all(chunk)
                    .await
                    .map_err(|_| TransportError::Closed)?;
            }
            self.tx.flush().await.map_err(|_| TransportError::Closed)
        }))
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

// ===========================================================================
// Dialer / Listener
// ===========================================================================

/// The initiating (client) side. Holds the UART halves and hands them to the one
/// connection it ever opens (the peripheral is moved in, so it can't redial —
/// pair with `ClientConfig { reconnect: false, .. }`, the [`SerialClient`] default).
pub struct SerialDialer<Rd, Wr> {
    halves: RefCell<Option<(Rd, Wr)>>,
}

// SAFETY: single-core cooperative Embassy executor — see the connection above.
unsafe impl<Rd, Wr> Send for SerialDialer<Rd, Wr> {}

impl<Rd, Wr> SerialDialer<Rd, Wr> {
    /// Build a one-shot dialer over the split UART halves.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self {
            halves: RefCell::new(Some((rx, tx))),
        }
    }
}

impl<Rd, Wr> Dialer for SerialDialer<Rd, Wr>
where
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            let (rx, tx) = self.halves.borrow_mut().take().ok_or(TransportError::Io)?;
            Ok(Box::new(EmbassySerialConnection::new(rx, tx)) as Box<dyn Connection>)
        }))
    }
}

/// The accepting (server) side. Serial is point-to-point: the first
/// [`accept`](Listener::accept) hands out the connection; later calls park forever.
pub struct SerialListener<Rd, Wr> {
    halves: Option<(Rd, Wr)>,
}

// SAFETY: single-core cooperative Embassy executor — see the connection above.
unsafe impl<Rd, Wr> Send for SerialListener<Rd, Wr> {}

impl<Rd, Wr> SerialListener<Rd, Wr> {
    /// Wrap the split UART halves as a one-shot listener.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self {
            halves: Some((rx, tx)),
        }
    }
}

impl<Rd, Wr> Listener for SerialListener<Rd, Wr>
where
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            match self.halves.take() {
                Some((rx, tx)) => {
                    Ok(Box::new(EmbassySerialConnection::new(rx, tx)) as Box<dyn Connection>)
                }
                // Point-to-point: no second peer ever arrives.
                None => core::future::pending().await,
            }
        }))
    }
}

// ===========================================================================
// Client sugar (hand-rolled ConnectorBuilder)
// ===========================================================================

/// Mirrors records to/from an AimX peer over a serial UART. Register it via
/// `with_connector`; declare the routes with `link_to`/`link_from` on the
/// `serial://` scheme (or override with [`scheme`](Self::scheme)).
pub struct SerialClient<Rd, Wr> {
    halves: RefCell<Option<(Rd, Wr)>>,
    config: ClientConfig,
    scheme: String,
}

// SAFETY: single-core cooperative Embassy executor — see the connection above.
// `ConnectorBuilder: Send + Sync`, so the builder must assert both.
unsafe impl<Rd, Wr> Send for SerialClient<Rd, Wr> {}
unsafe impl<Rd, Wr> Sync for SerialClient<Rd, Wr> {}

impl<Rd, Wr> SerialClient<Rd, Wr> {
    /// Build a client over the split UART halves (e.g. from `Uart::split()`).
    ///
    /// Reconnect is disabled by default: the peripheral is moved in and can't be
    /// re-acquired after a drop. Override with [`with_config`](Self::with_config).
    pub fn new(rx: Rd, tx: Wr) -> Self {
        let config = ClientConfig {
            reconnect: false,
            ..ClientConfig::default()
        };
        Self {
            halves: RefCell::new(Some((rx, tx))),
            config,
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }

    /// Override the client engine config (keepalive, offline queue, …). Note that
    /// re-enabling `reconnect` cannot re-open the moved-in UART.
    pub fn with_config(mut self, config: ClientConfig) -> Self {
        self.config = config;
        self
    }
}

impl<R, Rd, Wr> ConnectorBuilder<R> for SerialClient<Rd, Wr>
where
    R: TimeOps + 'static,
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb<R>) -> BuildFuture<'a> {
        Box::pin(SendFutureWrapper(async move {
            let (rx, tx) = self
                .halves
                .borrow_mut()
                .take()
                .ok_or_else(connector_consumed)?;
            let dialer = SerialDialer::new(rx, tx);
            let (handle, engine) =
                run_client(dialer, AimxCodec, self.config.clone(), db.runtime_arc());
            // One pump future per route; each holds a `ClientHandle` clone, so the
            // engine stays alive as long as any mirror runs.
            let mut futures = pump_client(db, &self.scheme, &handle);
            futures.push(engine);
            Ok(futures)
        }))
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

// ===========================================================================
// Server sugar (hand-rolled ConnectorBuilder)
// ===========================================================================

/// Serves the full AimX toolset over a serial UART, so a host (or another board)
/// can `record.list`/`get`/`set`/`subscribe`/`drain` this db over the wire.
pub struct SerialServer<Rd, Wr> {
    halves: RefCell<Option<(Rd, Wr)>>,
    config: AimxConfig,
    scheme: String,
}

// SAFETY: single-core cooperative Embassy executor — see the connection above.
unsafe impl<Rd, Wr> Send for SerialServer<Rd, Wr> {}
unsafe impl<Rd, Wr> Sync for SerialServer<Rd, Wr> {}

impl<Rd, Wr> SerialServer<Rd, Wr> {
    /// Serve AimX over the split UART halves, with the default read-only policy.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self {
            halves: RefCell::new(Some((rx, tx))),
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

    /// Maximum live subscriptions for the connection.
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

impl<R, Rd, Wr> ConnectorBuilder<R> for SerialServer<Rd, Wr>
where
    R: RuntimeAdapter + 'static,
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb<R>) -> BuildFuture<'a> {
        Box::pin(SendFutureWrapper(async move {
            let (rx, tx) = self
                .halves
                .borrow_mut()
                .take()
                .ok_or_else(connector_consumed)?;
            let listener = SerialListener::new(rx, tx);

            // Apply the security policy's writable marking so `record.list` reports
            // the `writable` flag (the dispatch also enforces it).
            crate::apply_writable(db, &self.config);

            let session_config = SessionConfig {
                limits: SessionLimits {
                    // A UART carries a single peer.
                    max_connections: 1,
                    max_subs_per_connection: self.config.max_subs_per_connection,
                },
                reads_hello: false,
                // AimX's subscribe ack stays implicit (events flow); no ack frame.
                acks_subscribe: false,
            };
            let dispatch: Arc<dyn Dispatch> =
                Arc::new(AimxDispatch::new(Arc::new(db.clone()), self.config.clone()));
            let fut: BoxFuture = Box::pin(serve(
                listener,
                Arc::new(AimxCodec),
                dispatch,
                session_config,
            ));
            Ok(vec![fut])
        }))
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// The builder's UART halves were already taken — `build` ran twice. The
/// framework calls it once, so this is unreachable in practice.
fn connector_consumed() -> DbError {
    DbError::MissingConfiguration {
        #[cfg(feature = "std")]
        parameter: String::from("serial connector already built"),
        #[cfg(not(feature = "std"))]
        _parameter: (),
    }
}
