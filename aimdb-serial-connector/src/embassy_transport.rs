//! Embassy serial transport (feature `embassy-runtime`, `no_std + alloc`) — thin
//! sugar over the centralized Embassy session spine in `aimdb-embassy-adapter`.
//!
//! This half contributes **only** the COBS [`Framer`] plus thin sugar; the framed
//! [`Connection`](aimdb_core::session::Connection), the one-shot
//! dialer/listener/cell, and the force-`Send` plumbing all live in
//! [`aimdb_embassy_adapter::connectors`]. So this module carries **no `unsafe`**
//! (down from the seven `unsafe impl`s this half used to hand-roll) — the Embassy
//! half is now structurally a sibling of the [Tokio half](crate::tokio_transport),
//! both thin sugar over a shared spine.
//!
//! Generic over the `embedded-io-async` `Read`/`Write` halves (the common Embassy
//! HAL shape, e.g. `Uart::split()`), so it works with any chip's async UART.

use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use embedded_io_async::{Read, Write};

use aimdb_embassy_adapter::connectors::{
    EmbassyConnection, EmbassySessionClient, Framer, OneShotCell, OneShotDialer, OneShotListener,
};

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{serve, Dispatch, SessionConfig, SessionLimits};
use aimdb_core::{AimDb, DbResult};

use crate::framing::{encode_frame, FrameAccumulator};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// How many bytes a single UART `read` pulls before re-checking for a frame.
const READ_CHUNK: usize = 64;
/// Max bytes per `write` call. Some HAL `BufferedUart::write` is atomic-or-error
/// (e.g. `embassy-stm32` returns `BufferTooLong` for a single write larger than
/// its TX ring), so a frame bigger than the buffer must be split.
const WRITE_CHUNK: usize = 64;

/// The framed connection type a serial peripheral produces: COBS over the UART.
type SerialConnection<Rd, Wr> = EmbassyConnection<Rd, Wr, CobsFramer, READ_CHUNK, WRITE_CHUNK>;

// ===========================================================================
// COBS framer — the only serial-specific transport bit.
// ===========================================================================

/// COBS framing for the Embassy [`EmbassyConnection`]: `encode` COBS-encodes a
/// frame and appends the `0x00` sentinel; the accumulator yields one frame per
/// sentinel (a malformed run is skipped — COBS is self-synchronizing).
pub struct CobsFramer {
    acc: FrameAccumulator,
}

impl CobsFramer {
    /// A fresh COBS framer.
    pub fn new() -> Self {
        Self {
            acc: FrameAccumulator::new(),
        }
    }
}

impl Default for CobsFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer for CobsFramer {
    fn encode(&self, frame: &[u8], out: &mut Vec<u8>) {
        encode_frame(frame, out);
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.acc.push_bytes(bytes);
    }

    fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>> {
        // The accumulator's `FrameError` collapses to `()`: the connection only
        // distinguishes "got a frame" from "skip and resync".
        self.acc.next_frame().map(|r| r.map_err(|_| ()))
    }
}

// ===========================================================================
// Client sugar — one-shot dial over the moved-in UART.
// ===========================================================================

/// Constructs an [`EmbassySessionClient`] that mirrors records to/from an AimX
/// peer over a serial UART. `SerialClient::new(rx, tx)` is sugar; chain
/// `.scheme(...)` / `.with_config(...)` on the returned connector and register it
/// with `with_connector`.
///
/// Reconnect is disabled by default: the peripheral is moved in and can't be
/// re-acquired after a drop.
pub struct SerialClient;

impl SerialClient {
    /// Mirror records to/from the AimX peer over the split UART halves (e.g. from
    /// `Uart::split()`). Scheme defaults to [`DEFAULT_SCHEME`].
    // Sugar constructor: intentionally returns the spine connector, not `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<Rd, Wr>(
        rx: Rd,
        tx: Wr,
    ) -> EmbassySessionClient<OneShotDialer<SerialConnection<Rd, Wr>>, AimxCodec>
    where
        Rd: Read + 'static,
        Wr: Write + 'static,
    {
        let conn = EmbassyConnection::new(rx, tx, CobsFramer::new());
        // Reconnect stays disabled (the spine's default): the UART peripheral is
        // moved in and can't be re-acquired.
        EmbassySessionClient::new(OneShotDialer::new(conn), AimxCodec).scheme(DEFAULT_SCHEME)
    }
}

// ===========================================================================
// Server sugar — serve the full AimX toolset over the moved-in UART.
// ===========================================================================

/// Serves the full AimX toolset over a serial UART, so a host (or another board)
/// can `record.list`/`get`/`set`/`subscribe`/`drain` this db over the wire.
/// Register it directly with `with_connector`:
///
/// ```ignore
/// builder.with_connector(
///     SerialServer::new(rx, tx).security_policy(SecurityPolicy::read_only()),
/// );
/// ```
///
/// Holds the moved-in framed UART connection (built up front from the halves) in
/// the adapter's force-`Send + Sync` [`OneShotCell`]; `build` takes it, hands it
/// to a [`OneShotListener`], and drives `serve`. Storing it in the cell (rather
/// than a bare `RefCell`) keeps **all** the `unsafe` in the adapter — this crate
/// has none.
pub struct SerialServer<Rd, Wr> {
    conn: OneShotCell<SerialConnection<Rd, Wr>>,
    config: AimxConfig,
    scheme: String,
}

impl<Rd, Wr> SerialServer<Rd, Wr>
where
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    /// Serve AimX over the split UART halves, with the default read-only policy.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self {
            conn: OneShotCell::new(EmbassyConnection::new(rx, tx, CobsFramer::new())),
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }
}

impl<Rd, Wr> SerialServer<Rd, Wr> {
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

impl<Rd, Wr> ConnectorBuilder for SerialServer<Rd, Wr>
where
    Rd: Read + 'static,
    Wr: Write + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        // Take the moved-in connection out of `&self` (build runs once); the
        // canonical "already built" error lives on the adapter's cell.
        let conn = self.conn.take_required();
        let config = self.config.clone();
        Box::pin(async move {
            let conn = conn?;
            // Apply the security policy's writable marking so `record.list` reports
            // the `writable` flag (the dispatch also enforces it).
            crate::apply_writable(db, &config);
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
            let dispatch: Arc<dyn Dispatch> =
                Arc::new(AimxDispatch::new(Arc::new(db.clone()), config));
            // `serve` is `Send` here: the one-shot listener + framed connection
            // force-`Send` their futures inside the adapter.
            let fut: BoxFuture = Box::pin(serve(
                OneShotListener::new(conn),
                Arc::new(AimxCodec),
                dispatch,
                session_config,
            ));
            Ok(vec![fut])
        })
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}
