//! Generic, transport-agnostic session connectors — the reusable spine every
//! transport crate (`aimdb-uds-connector`, and later serial/TCP) wraps.
//!
//! A transport contributes only a [`Dialer`]/[`Listener`]/[`Connection`] triple
//! and an [`EnvelopeCodec`]; the engine wiring (reconnect, pumps, accept loop,
//! fan-out) is inherited here, so a new transport is a thin crate and swapping
//! one never ripples into record/link code.
//!
//! - [`SessionClientConnector`] — the dialing half: on `build` it opens
//!   [`run_client`] over the injected dialer/codec and drives [`pump_client`] for
//!   every route under its **scheme**.
//! - [`SessionServerConnector`] — the accepting half: it binds a [`Listener`]
//!   (behind a factory, so bind errors surface synchronously from `build`) and
//!   drives [`serve`] with an injected dispatch + codec.
//!
//! The **scheme** is a constructor argument (default `"remote"`) decoupling the
//! logical routing key from the transport, so two transports can coexist under
//! different schemes.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use aimdb_executor::{RuntimeAdapter, TimeOps};

use crate::builder::AimDb;
use crate::connector::ConnectorBuilder;
use crate::session::{
    pump_client, run_client, serve, ClientConfig, Dialer, Dispatch, EnvelopeCodec, Listener,
    SessionConfig,
};
use crate::DbResult;

/// The default scheme a session connector registers when none is given.
pub const DEFAULT_SCHEME: &str = "remote";

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

// ===========================================================================
// Client — dials a peer, mirrors records under `scheme`.
// ===========================================================================

/// Mirrors records to/from a peer reached via the dialer `D`, speaking codec `C`,
/// under a logical [`scheme`](ConnectorBuilder::scheme). A transport crate wraps
/// it in a one-line sugar constructor (e.g. `UdsClient`).
pub struct SessionClientConnector<D, C> {
    scheme: String,
    dialer: D,
    codec: C,
    config: ClientConfig,
}

impl<D, C> SessionClientConnector<D, C> {
    /// Mirror records over `dialer`, framing messages with `codec`. The scheme
    /// defaults to `"remote"`.
    pub fn new(dialer: D, codec: C) -> Self {
        Self {
            scheme: DEFAULT_SCHEME.to_string(),
            dialer,
            codec,
            config: ClientConfig::default(),
        }
    }

    /// Override the scheme this connector registers (so `<scheme>://<record>`
    /// links validate and route here).
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }

    /// Override the client engine config (reconnect policy, keepalive, etc.).
    pub fn with_config(mut self, config: ClientConfig) -> Self {
        self.config = config;
        self
    }
}

impl<R, D, C> ConnectorBuilder<R> for SessionClientConnector<D, C>
where
    R: TimeOps + 'static,
    D: Dialer + Clone + Send + Sync + 'static,
    C: EnvelopeCodec + Clone + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb<R>) -> BuildFuture<'a> {
        Box::pin(async move {
            let (handle, engine_fut) = run_client(
                self.dialer.clone(),
                self.codec.clone(),
                self.config.clone(),
                db.runtime_arc(),
            );
            // One pump future per route; each holds a `ClientHandle` clone, so the
            // engine stays alive as long as any mirror runs. `handle` drops here.
            let mut futures = pump_client(db, &self.scheme, &handle);
            futures.push(engine_fut);
            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

// ===========================================================================
// Server — accepts connections, serves a dispatch under `scheme`.
// ===========================================================================

/// Accepts connections from a [`Listener`] `L` and serves them with a dispatch,
/// speaking codec `C`, under a logical [`scheme`](ConnectorBuilder::scheme).
///
/// Two factories keep it transport- and protocol-agnostic:
/// - `listener_factory` runs at `build` time and returns `DbResult<L>`, so the
///   bind happens there and any error surfaces synchronously from `build`.
/// - `dispatch_factory` turns the live `&AimDb<R>` into an `Arc<dyn Dispatch>`
///   (e.g. an `AimxDispatch`), so the spine never names a concrete protocol.
pub struct SessionServerConnector<C, LF, DF> {
    scheme: String,
    listener_factory: LF,
    codec: C,
    dispatch_factory: DF,
    config: SessionConfig,
}

impl<C, LF, DF> SessionServerConnector<C, LF, DF> {
    /// Build a server connector. `listener_factory` binds the listener at
    /// `build` time; `dispatch_factory` produces the per-server dispatch from the
    /// live db. The scheme defaults to `"remote"`.
    pub fn new(
        listener_factory: LF,
        codec: C,
        dispatch_factory: DF,
        config: SessionConfig,
    ) -> Self {
        Self {
            scheme: DEFAULT_SCHEME.to_string(),
            listener_factory,
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

impl<R, L, C, LF, DF> ConnectorBuilder<R> for SessionServerConnector<C, LF, DF>
where
    R: RuntimeAdapter + 'static,
    L: Listener + 'static,
    C: EnvelopeCodec + Clone + 'static,
    LF: Fn() -> DbResult<L> + Send + Sync + 'static,
    DF: Fn(&AimDb<R>) -> Arc<dyn Dispatch> + Send + Sync + 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb<R>) -> BuildFuture<'a> {
        // Bind synchronously so a bind error surfaces from `build`.
        let listener = (self.listener_factory)();
        let dispatch = (self.dispatch_factory)(db);
        let codec = Arc::new(self.codec.clone());
        let config = self.config.clone();
        Box::pin(async move {
            let listener = listener?;
            let fut: BoxFuture = Box::pin(serve(listener, codec, dispatch, config));
            Ok(vec![fut])
        })
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}
