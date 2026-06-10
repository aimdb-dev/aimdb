//! Centralized Embassy connector spines — the one audited home for the
//! single-core `unsafe` + [`SendFutureWrapper`](crate::SendFutureWrapper) that
//! every Embassy connector used to hand-roll.
//!
//! AimDB's connector contract is `Send`-everywhere (so a Tokio app can
//! `tokio::spawn(runner.run())`). Embassy's primitives (UART halves over
//! `embedded-io-async`, channels over `NoopRawMutex`, …) are `!Send` *by design*
//! — single-core, cooperative, no preemption or thread migration. Bridging the
//! two requires force-`Send`ing the Embassy futures; this module does that
//! **once**, so a connector crate carries **no `unsafe` and no wrapper**:
//!
//! - **Session transports** (serial, TCP, …) contribute a [`Framer`] (or a
//!   [`Connection`]) and wrap it in [`EmbassySessionClient`] /
//!   [`EmbassySessionServer`] — the Embassy duals of core's
//!   `SessionClientConnector` / `SessionServerConnector`.
//! - **Data-plane transports** (MQTT, KNX) contribute an [`EmbassySinkRaw`]
//!   (outbound) and/or [`EmbassySourceRaw`] (inbound) and ride core's existing
//!   [`pump_sink`](aimdb_core::session::pump_sink) /
//!   [`pump_source`](aimdb_core::session::pump_source) via the force-`Send`
//!   bridges [`EmbassySink`] / [`EmbassySource`].
//!
//! # Safety invariant (shared by every `unsafe impl` below)
//!
//! An Embassy executor runs cooperatively on a single core with no preemption or
//! thread migration, so the wrapped `!Send` values are never actually accessed
//! from another thread. Only use these spines under an Embassy executor.

use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::session::{
    pump_client, run_client, serve, BoxFut, ClientConfig, Connection, Dialer, Dispatch,
    EnvelopeCodec, Listener, Payload, SessionConfig, Source, TransportError, TransportResult,
};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{AimDb, DbError, DbResult};

use crate::SendFutureWrapper;

/// The scheme a spine registers when the connector gives none (matches core's
/// `SessionClientConnector` default).
pub const DEFAULT_SCHEME: &str = "remote";

/// The runner's collected future type (`Send`, as the std contract requires).
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// The `ConnectorBuilder::build` return shape.
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// The spine's one-shot peripheral was already consumed — `build` ran twice. The
/// framework calls it once, so this is unreachable in practice.
fn connector_consumed() -> DbError {
    DbError::missing_configuration("embassy connector already built")
}

// ===========================================================================
// Data-plane bridges — let a `!Send` sink/source ride core's pumps.
// ===========================================================================

/// The pure outbound I/O a data-plane connector contributes: publish one payload.
/// The `!Send` dual of [`Connector`]; [`EmbassySink`] force-`Send`s it so it can
/// drive core's [`pump_sink`](aimdb_core::session::pump_sink).
///
/// Args are owned (a data-plane sink enqueues owned data onto its channel anyway),
/// so the returned future borrows only `&self` — matching [`Connector::publish`]'s
/// `'_` return shape.
pub trait EmbassySinkRaw {
    /// Publish `payload` to `destination` (e.g. enqueue onto an Embassy channel).
    fn publish(
        &self,
        destination: String,
        config: ConnectorConfig,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<(), PublishError>>;
}

/// Force-`Send` bridge turning an [`EmbassySinkRaw`] into a [`Connector`], so an
/// Embassy outbound sink rides core's [`pump_sink`](aimdb_core::session::pump_sink)
/// unchanged.
pub struct EmbassySink<C>(pub C);

// SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
unsafe impl<C> Send for EmbassySink<C> {}
// SAFETY: same invariant; `Connector` is shared behind `Arc<dyn Connector>`.
unsafe impl<C> Sync for EmbassySink<C> {}

impl<C: EmbassySinkRaw> Connector for EmbassySink<C> {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Own the args so the inner future borrows only `&self` (see trait doc).
        Box::pin(SendFutureWrapper(self.0.publish(
            destination.to_string(),
            config.clone(),
            payload.to_vec(),
        )))
    }
}

/// The pure inbound I/O a data-plane connector contributes: yield the next
/// `(topic, payload)`. The `!Send` dual of [`Source`]; [`EmbassySource`]
/// force-`Send`s it so it can drive core's
/// [`pump_source`](aimdb_core::session::pump_source).
pub trait EmbassySourceRaw {
    /// Yield the next `(topic, payload)`, or `None` when the source is done.
    fn next(&mut self) -> impl Future<Output = Option<(String, Payload)>>;
}

/// Force-`Send` bridge turning an [`EmbassySourceRaw`] into a [`Source`], so an
/// Embassy inbound stream rides core's
/// [`pump_source`](aimdb_core::session::pump_source) unchanged.
pub struct EmbassySource<S>(pub S);

// SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
unsafe impl<S> Send for EmbassySource<S> {}

impl<S: EmbassySourceRaw> Source for EmbassySource<S> {
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(SendFutureWrapper(self.0.next()))
    }
}

/// Force-`Send` + box a connector's long-lived **protocol task** (an MQTT broker
/// manager, a KNX tunnelling state machine, …) so it can join the runner's
/// `Send` future set without the connector touching [`SendFutureWrapper`].
pub fn into_box_future<F>(fut: F) -> BoxFuture
where
    F: Future<Output = ()> + 'static,
{
    Box::pin(SendFutureWrapper(fut))
}

/// Force-`Send + Sync` handle to the Embassy network stack.
///
/// `embassy_net::Stack` is `!Sync` (internal `RefCell`), so a
/// [`ConnectorBuilder`] (which must be `Send + Sync`) cannot hold the bare
/// `&'static Stack`. Network connectors (MQTT, KNX) take the stack at
/// construction and wrap it here — keeping the single-core `unsafe` in this
/// audited module instead of in every connector crate. Replaces the deleted
/// `EmbassyNetwork` runtime trait (issue #131: the runtime travels as
/// `Arc<dyn RuntimeOps>`, which cannot surface adapter-specific capabilities).
#[cfg(feature = "embassy-net-support")]
#[derive(Clone, Copy)]
pub struct NetStack(&'static embassy_net::Stack<'static>);

// SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
#[cfg(feature = "embassy-net-support")]
unsafe impl Send for NetStack {}
// SAFETY: same invariant; the stack's `RefCell` is never borrowed from another thread.
#[cfg(feature = "embassy-net-support")]
unsafe impl Sync for NetStack {}

#[cfg(feature = "embassy-net-support")]
impl NetStack {
    /// Wrap the device's network stack for storage inside a connector builder.
    ///
    /// # Safety
    ///
    /// `embassy_net::Stack` is `!Sync` (internal `RefCell`), and `NetStack`
    /// force-implements `Send + Sync` on top of it. The caller must uphold the
    /// module-level invariant: every future that touches this stack —
    /// including the connector protocol task the builder spawns — is polled on
    /// the same single-core cooperative executor. Constructing one on a
    /// multicore / multi-executor setup (a second core's executor or an
    /// interrupt executor also driving network futures) is undefined behavior.
    pub unsafe fn new(stack: &'static embassy_net::Stack<'static>) -> Self {
        Self(stack)
    }

    /// The wrapped stack reference.
    pub fn get(&self) -> &'static embassy_net::Stack<'static> {
        self.0
    }
}

// ===========================================================================
// Session spine — the Embassy duals of `SessionClientConnector` / `…Server`.
// ===========================================================================

/// A force-`Send + Sync` one-shot cell. Holds a moved-in value behind interior
/// mutability so a [`ConnectorBuilder`] (which is `Send + Sync`) can take it once
/// from `&self` in `build` — without the connector crate writing any `unsafe`.
///
/// Use it when a session-server connector holds a moved-in peripheral/connection
/// it hands to a [`OneShotListener`] at build time (the moved-in dual of the
/// Tokio server's re-bindable listener factory).
pub struct OneShotCell<C> {
    inner: RefCell<Option<C>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
unsafe impl<C> Send for OneShotCell<C> {}
// SAFETY: same invariant; the `RefCell` is never borrowed from another thread.
unsafe impl<C> Sync for OneShotCell<C> {}

impl<C> OneShotCell<C> {
    /// Hold `value` for a single [`take`](Self::take).
    pub fn new(value: C) -> Self {
        Self {
            inner: RefCell::new(Some(value)),
        }
    }

    /// Take the value, or `None` if already taken (i.e. `build` ran twice).
    pub fn take(&self) -> Option<C> {
        self.inner.borrow_mut().take()
    }

    /// Take the value, or the canonical "already built" [`DbError`] — the shared
    /// error every spine returns when `build` is (impossibly) called twice.
    pub fn take_required(&self) -> DbResult<C> {
        self.take().ok_or_else(connector_consumed)
    }
}

/// One-shot [`Dialer`] over a pre-built, moved-in `Connection` (an Embassy
/// peripheral can't be re-acquired, so it dials exactly once; keep reconnect
/// disabled — [`EmbassySessionClient::new`]'s default).
pub struct OneShotDialer<C> {
    conn: OneShotCell<C>,
}

impl<C> OneShotDialer<C> {
    /// Wrap a pre-built connection to be handed out on the first `connect`.
    pub fn new(conn: C) -> Self {
        Self {
            conn: OneShotCell::new(conn),
        }
    }
}

impl<C: Connection + 'static> Dialer for OneShotDialer<C> {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            self.conn
                .take()
                .map(|c| Box::new(c) as Box<dyn Connection>)
                .ok_or(TransportError::Io)
        }))
    }
}

/// One-shot [`Listener`] over a pre-built, moved-in `Connection`: the first
/// `accept` hands it out; later calls park forever (point-to-point peripheral).
pub struct OneShotListener<C> {
    conn: Option<C>,
}

// SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
unsafe impl<C> Send for OneShotListener<C> {}

impl<C> OneShotListener<C> {
    /// Wrap a pre-built connection to be handed out on the first `accept`.
    pub fn new(conn: C) -> Self {
        Self { conn: Some(conn) }
    }
}

impl<C: Connection + 'static> Listener for OneShotListener<C> {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            match self.conn.take() {
                Some(c) => Ok(Box::new(c) as Box<dyn Connection>),
                // Point-to-point: no second peer ever arrives.
                None => core::future::pending().await,
            }
        }))
    }
}

/// Embassy dual of `SessionClientConnector`: dials a peer with `D`, speaks codec
/// `C`, mirrors records under a [`scheme`](ConnectorBuilder::scheme). A transport
/// crate wraps it in a one-line sugar constructor (e.g. `SerialClient`).
pub struct EmbassySessionClient<D, C> {
    scheme: String,
    // The moved-in dialer behind the force-`Send + Sync` cell, so the builder is
    // auto `Send + Sync` (with a `Send + Sync` codec) — no `unsafe` here.
    dialer: OneShotCell<D>,
    codec: C,
    config: ClientConfig,
}

impl<D, C> EmbassySessionClient<D, C> {
    /// Mirror records over `dialer`, framing with `codec`. Scheme defaults to
    /// [`DEFAULT_SCHEME`].
    ///
    /// Reconnect is **disabled** by default (unlike [`ClientConfig::default`]):
    /// an Embassy dialer typically wraps a moved-in peripheral ([`OneShotDialer`])
    /// that can't be re-acquired, so redialing would spin on [`TransportError::Io`]
    /// forever. A transport whose dialer really can redial opts back in via
    /// [`with_config`](Self::with_config).
    pub fn new(dialer: D, codec: C) -> Self {
        Self {
            scheme: DEFAULT_SCHEME.to_string(),
            dialer: OneShotCell::new(dialer),
            codec,
            config: ClientConfig {
                reconnect: false,
                ..ClientConfig::default()
            },
        }
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }

    /// Override the client engine config (reconnect, keepalive, …). Only enable
    /// `reconnect` if the dialer can actually redial (a [`OneShotDialer`] can't).
    pub fn with_config(mut self, config: ClientConfig) -> Self {
        self.config = config;
        self
    }
}

impl<D, C> ConnectorBuilder for EmbassySessionClient<D, C>
where
    D: Dialer + 'static,
    C: EnvelopeCodec + Clone + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        Box::pin(SendFutureWrapper(async move {
            let dialer = self.dialer.take_required()?;
            let (handle, engine) = run_client(
                dialer,
                self.codec.clone(),
                self.config.clone(),
                db.runtime_ops(),
            );
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

/// Embassy dual of `SessionServerConnector`: serves a moved-in [`Listener`] with
/// a dispatch produced from the live db, speaking codec `C`, under a
/// [`scheme`](ConnectorBuilder::scheme).
pub struct EmbassySessionServer<L, C, DF> {
    scheme: String,
    // Moved-in listener behind the force-`Send + Sync` cell, so the builder is
    // auto `Send + Sync` (with a `Send + Sync` codec + factory) — no `unsafe` here.
    listener: OneShotCell<L>,
    codec: C,
    dispatch_factory: DF,
    config: SessionConfig,
}

impl<L, C, DF> EmbassySessionServer<L, C, DF> {
    /// Serve `listener` with the dispatch built by `dispatch_factory` from the
    /// live db, framing with `codec`. Scheme defaults to [`DEFAULT_SCHEME`].
    pub fn new(listener: L, codec: C, dispatch_factory: DF, config: SessionConfig) -> Self {
        Self {
            scheme: DEFAULT_SCHEME.to_string(),
            listener: OneShotCell::new(listener),
            codec,
            dispatch_factory,
            config,
        }
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

impl<L, C, DF> ConnectorBuilder for EmbassySessionServer<L, C, DF>
where
    L: Listener + 'static,
    C: EnvelopeCodec + Clone + 'static,
    DF: Fn(&AimDb) -> Arc<dyn Dispatch> + Send + Sync,
{
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        Box::pin(SendFutureWrapper(async move {
            let listener = self.listener.take_required()?;
            let dispatch = (self.dispatch_factory)(db);
            let codec = Arc::new(self.codec.clone());
            // `serve` is `Send` here because the listener/connections force-`Send`
            // their futures; no extra wrapper needed.
            let fut: BoxFuture = Box::pin(serve(listener, codec, dispatch, self.config.clone()));
            Ok(vec![fut])
        }))
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

// ===========================================================================
// Framed connection over `embedded-io-async` — lets a session transport ship
// just a `Framer` and carry zero `unsafe`.
// ===========================================================================

#[cfg(feature = "connector-io")]
mod framed {
    use super::*;
    use embedded_io_async::{Read, Write};

    use aimdb_core::session::PeerInfo;

    /// Frames the byte stream a [`EmbassyConnection`] carries: a transport
    /// contributes only this (e.g. COBS, length-prefix), inheriting the
    /// force-`Send` plumbing.
    pub trait Framer {
        /// Encode one logical frame, appending its wire bytes to `out`.
        fn encode(&self, frame: &[u8], out: &mut Vec<u8>);
        /// Feed received bytes into the accumulator.
        fn push_bytes(&mut self, bytes: &[u8]);
        /// Pull the next complete frame: `Some(Ok(frame))`, `Some(Err(()))` for a
        /// malformed/unsynced run (skipped, the stream resyncs), or `None`.
        fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>>;
    }

    /// A framed bidirectional [`Connection`] over an `embedded-io-async` UART (or
    /// any `Read`/`Write` halves), force-`Send`ing its `recv`/`send` futures.
    /// `RC`/`WC` cap the per-`read`/`write` chunk (UART ring sizes).
    pub struct EmbassyConnection<Rd, Wr, F, const RC: usize = 64, const WC: usize = 64> {
        rx: Rd,
        tx: Wr,
        framer: F,
        peer: PeerInfo,
    }

    // SAFETY: single-core cooperative Embassy executor — see the module-level invariant.
    unsafe impl<Rd, Wr, F, const RC: usize, const WC: usize> Send
        for EmbassyConnection<Rd, Wr, F, RC, WC>
    {
    }

    impl<Rd, Wr, F, const RC: usize, const WC: usize> EmbassyConnection<Rd, Wr, F, RC, WC> {
        /// Wrap the split read/write halves of an async byte stream with `framer`.
        pub fn new(rx: Rd, tx: Wr, framer: F) -> Self {
            Self {
                rx,
                tx,
                framer,
                peer: PeerInfo::default(),
            }
        }
    }

    impl<Rd, Wr, F, const RC: usize, const WC: usize> Connection
        for EmbassyConnection<Rd, Wr, F, RC, WC>
    where
        Rd: Read,
        Wr: Write,
        F: Framer,
    {
        fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
            Box::pin(SendFutureWrapper(async move {
                loop {
                    // A run that fails to decode is line noise or a mid-stream
                    // join, not fatal: skip it and resync on the next frame.
                    match self.framer.next_frame() {
                        Some(Ok(frame)) => return Ok(Some(frame)),
                        Some(Err(())) => continue,
                        None => {}
                    }
                    let mut chunk = [0u8; RC];
                    match self.rx.read(&mut chunk).await {
                        Ok(0) => return Ok(None), // EOF — peer closed
                        Ok(n) => self.framer.push_bytes(&chunk[..n]),
                        Err(_) => return Err(TransportError::Io),
                    }
                }
            }))
        }

        fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
            Box::pin(SendFutureWrapper(async move {
                let mut out = Vec::new();
                self.framer.encode(frame, &mut out);
                // Some HAL `BufferedUart::write` rejects a single write larger than
                // its TX ring, so split into `WC`-sized chunks.
                for chunk in out.chunks(WC) {
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
}

#[cfg(feature = "connector-io")]
pub use framed::{EmbassyConnection, Framer};
