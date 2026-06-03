//! `SendFutureWrapper` — force-`Send` an Embassy future for AimDB's connector spine.
//!
//! AimDB's connector framework requires `Send` futures: `ConnectorBuilder::build`
//! returns `Vec<Pin<Box<dyn Future + Send>>>` and `AimDbRunner` drives them on a
//! `Send` `BoxFuture`. Embassy's primitives (channels over `NoopRawMutex`, …) are
//! `!Send` *by design* — single-core, cooperative, no preemption or thread
//! migration — so an Embassy connector's data-plane futures must be force-`Send`ed
//! to satisfy that bound.
//!
//! This is also *why* Embassy data-plane connectors hand-roll their outbound /
//! inbound loops instead of riding core's `pump_sink` / `pump_source`: those need a
//! `Send + Sync` `Connector` / `Send` `Source`, which `!Send` Embassy channels
//! cannot be without force-`Send`ing every primitive.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Asserts `Send` for a future driven exclusively by an Embassy executor.
pub struct SendFutureWrapper<F>(pub F);

// SAFETY: Embassy executors run cooperatively on a single core with no preemption or
/// thread migration, so the wrapped value is never actually moved across threads.
/// Only wrap futures that are polled solely by an Embassy executor.
unsafe impl<F> Send for SendFutureWrapper<F> {}

impl<F: Future> Future for SendFutureWrapper<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: structural pin projection onto the single field; never moved out.
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}
