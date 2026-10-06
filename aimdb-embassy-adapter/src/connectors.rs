//! The one audited home for the single-core `unsafe` that Embassy connectors
//! need.
//!
//! AimDB's connector contract is `Send`-everywhere (so a Tokio app can
//! `tokio::spawn(runner.run())`). Embassy's primitives (channels over
//! `NoopRawMutex`, a borrowed `embassy_net::Stack`, …) are `!Send` *by design* —
//! single-core, cooperative, no preemption or thread migration. Bridging the two
//! requires force-`Send`ing the Embassy futures; this module does that **once**,
//! so a connector crate carries **no `unsafe` and no wrapper**: it boxes its
//! protocol task with [`into_box_future`] and holds the network stack as a
//! `NetStack`.
//!
//! Session transports (serial, TCP, …) do not come through here. They ride
//! core's runtime-neutral spine directly — `SessionClientConnector` /
//! `SessionServerConnector` over `FramedConnection`, with the byte source from
//! this crate's `io` or `net` module (unlinked: neither exists in a
//! `connectors`-only build).
//!
//! # Safety invariant (shared by every `unsafe impl` below)
//!
//! An Embassy executor runs cooperatively on a single core with no preemption or
//! thread migration, so the wrapped `!Send` values are never actually accessed
//! from another thread. Only use these bridges under an Embassy executor.

use core::future::Future;
use core::pin::Pin;

use alloc::boxed::Box;

use crate::SendFutureWrapper;

/// The runner's collected future type (`Send`, as the std contract requires).
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

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
