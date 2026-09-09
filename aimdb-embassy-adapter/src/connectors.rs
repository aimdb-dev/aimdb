//! The data-plane bridge — the one audited home for the single-core `unsafe` +
//! [`SendFutureWrapper`] that every Embassy data-plane connector used to
//! hand-roll.
//!
//! AimDB's connector contract is `Send`-everywhere (so a Tokio app can
//! `tokio::spawn(runner.run())`). Embassy's primitives (channels over
//! `NoopRawMutex`, a borrowed `embassy_net::Stack`, …) are `!Send` *by design* —
//! single-core, cooperative, no preemption or thread migration. Bridging the two
//! requires force-`Send`ing the Embassy futures; this module does that **once**,
//! so a connector crate carries **no `unsafe` and no wrapper**.
//!
//! Data-plane transports (MQTT, KNX) contribute an [`EmbassySinkRaw`] (outbound)
//! and/or [`EmbassySourceRaw`] (inbound) and ride core's
//! [`pump_sink`](aimdb_core::session::pump_sink) /
//! [`pump_source`](aimdb_core::session::pump_source) via the force-`Send`
//! bridges [`EmbassySink`] / [`EmbassySource`].
//!
//! Session transports (serial, TCP, …) no longer come through here. They ride
//! core's runtime-neutral spine directly — `SessionClientConnector` /
//! `SessionServerConnector` over `FramedConnection`, with the byte source from
//! this crate's `io` or `net` module (unlinked: neither exists in a
//! `connectors`-only build) — so the Embassy duals this module used to
//! carry (`EmbassySessionClient`/`Server`, `EmbassyConnection`, `OneShotCell`
//! and the one-shot dialer/listener) are gone. Their one-shot semantics live in
//! core as `OneShot`, `OneShotDialer` and `OneShotListener`.
//!
//! # Safety invariant (shared by every `unsafe impl` below)
//!
//! An Embassy executor runs cooperatively on a single core with no preemption or
//! thread migration, so the wrapped `!Send` values are never actually accessed
//! from another thread. Only use these bridges under an Embassy executor.

use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use aimdb_core::session::{BoxFut, Payload, Source};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};

use crate::SendFutureWrapper;

/// The runner's collected future type (`Send`, as the std contract requires).
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

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
/// `ConnectorBuilder` (which must be `Send + Sync`) cannot hold the bare
/// `&'static Stack`. Network connectors (MQTT, KNX) take the stack at
/// construction and wrap it here — keeping the single-core `unsafe` in this
/// audited module instead of in every connector crate. Replaces the deleted
/// `EmbassyNetwork` runtime trait (the runtime travels as
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
