//! The runtime thread, as one value.
//!
//! `aimdb-sync` spawns an OS thread to own a Tokio runtime on the caller's
//! behalf. Everything that thread *is* — the way in, the database it built, and
//! the `fork` generation it belongs to — lives here, in one type.
//!
//! # Why the check lives on the way in
//!
//! [`Runtime::enter`] and [`Runtime::db`] are the only routes to the handle and
//! the database, and both refuse a runtime this process did not inherit a
//! thread for, so a publish or a read cannot be written that skips the check —
//! by construction rather than by convention, the treatment #231 gave
//! panic-freedom. It replaces a stamp copied into three types and guarded at
//! nine opt-in call sites, a shape that cost four bugs in one review pass,
//! every one of them someone forgetting to opt in.
//!
//! The check comes *before* the database is handed out, because a forked
//! child's `Arc<AimDb>` is perfectly valid — it came across with the address
//! space — so a child that reached it would publish into a buffer nobody drains
//! and be told `Ok`. That silence is the bug this mechanism exists to prevent.
//!
//! # Who owns it
//!
//! The handle owns the `Arc<Runtime>` and producers hold a
//! [`Weak`](alloc::sync::Weak), so the database dies with the handle and two
//! things stay free that shared ownership would have made expensive: a failed
//! upgrade *is* the liveness check, and dropping the database closes its
//! buffers, which is what wakes a consumer parked in `get()` — `aimdb-core` has
//! no explicit close. Consumers hold a [`RuntimeRef`] rather than a `Weak`, for
//! the reason given there.
//!
//! An `Arc` here was tried and reverted: it bought a producer outliving its
//! handle, cost both of the above (a liveness flag, a stop channel and a select
//! around every blocking read to rebuild), and left a forgotten producer
//! keeping an OS thread alive with nobody owning it — the stranded thread #232
//! had just removed.

use alloc::sync::{Arc, Weak};

use aimdb_core::AimDb;

use crate::error::{SyncError, SyncResult};

/// Signal to shut down the runtime thread.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShutdownSignal;

/// A runtime thread and everything reached through it.
pub(crate) struct Runtime {
    /// The way into the Tokio runtime. Reached only through [`Self::enter`].
    handle: tokio::runtime::Handle,

    /// The database the thread built. Reached only through [`Self::db`].
    ///
    /// Weak, because the strong reference lives in the handle's `OwnedThread`
    /// where a `shutdown(&self)` can release it — and releasing it closes the
    /// buffers, which is what wakes a parked consumer. A failed upgrade is
    /// therefore the same liveness check it always was, now firing for a
    /// shutdown as well as for a dropped handle.
    db: Weak<AimDb>,

    /// The fork generation this runtime's thread was spawned in.
    ///
    /// Plain data, which is the point: every refusal path is a unit test over a
    /// stale value — no thread, no `fork`.
    made_in: crate::fork::Generation,
}

impl Runtime {
    /// Borrowed, not taken: the caller keeps the strong reference so it has
    /// something to release. See [`Self::db`].
    pub(crate) fn new(handle: tokio::runtime::Handle, db: &Arc<AimDb>) -> Self {
        Self {
            handle,
            db: Arc::downgrade(db),
            made_in: crate::fork::generation(),
        }
    }

    /// Whether this process still owns the thread this runtime names.
    ///
    /// One relaxed atomic load — no syscall, no lock. It sits on the publish
    /// path, which is why it is not a `getpid` call.
    #[inline]
    pub(crate) fn check(&self) -> SyncResult<()> {
        if crate::fork::forked_since(self.made_in) {
            return Err(SyncError::ForkedChild);
        }
        Ok(())
    }

    /// The Tokio handle, or [`SyncError::ForkedChild`]. The only way to reach
    /// it: blocking on a runtime whose thread did not survive a `fork` would
    /// park forever.
    #[inline]
    pub(crate) fn enter(&self) -> SyncResult<&tokio::runtime::Handle> {
        self.check()?;
        Ok(&self.handle)
    }

    /// The database, or why there is none to reach — the only way in, so
    /// neither reason can be skipped. [`SyncError::ForkedChild`]: a child's
    /// handle to the database is valid, which is the problem.
    /// [`SyncError::RuntimeShutdown`]: the strong reference is gone.
    ///
    /// Owned, so one atomic increment per publish. A lock was the alternative.
    #[inline]
    pub(crate) fn db(&self) -> SyncResult<Arc<AimDb>> {
        self.check()?;
        self.db.upgrade().ok_or(SyncError::RuntimeShutdown)
    }

    /// A view of this runtime that may outlive it. The only way to build one.
    ///
    /// Checked, so a view is not a way around [`Self::enter`], and it copies
    /// the runtime's own stamp rather than reading the process's.
    pub(crate) fn view(&self) -> SyncResult<RuntimeRef> {
        Ok(RuntimeRef {
            handle: self.enter()?.clone(),
            made_in: self.made_in,
        })
    }
}

/// A borrowed view of a [`Runtime`] that outlives it on purpose.
///
/// Held by [`SyncConsumer`](crate::SyncConsumer), which has a requirement the
/// handle and producers do not: a `Reader` can still drain what is already
/// buffered after the runtime is gone, and the characterization tests pin that.
/// So a dead runtime must not refuse a read — but a `fork` still must.
///
/// Hence the generation is copied rather than reached through a
/// [`Weak`](alloc::sync::Weak), which cannot tell a detach here from a child
/// that released its inherited handle. The handle stays private to this module,
/// so `consumer.rs` reaches it only through [`Self::enter`].
pub(crate) struct RuntimeRef {
    handle: tokio::runtime::Handle,

    /// The runtime's own generation, copied at construction so this view keeps
    /// answering after that runtime is gone.
    made_in: crate::fork::Generation,
}

impl RuntimeRef {
    /// Refuse a forked child; let a detached one through.
    ///
    /// A fork is refused whether or not the child still holds the handle —
    /// what a `Weak` upgrade got wrong. A detach is not a fork; there the
    /// buffer answers for itself.
    #[inline]
    pub(crate) fn check(&self) -> SyncResult<()> {
        if crate::fork::forked_since(self.made_in) {
            return Err(SyncError::ForkedChild);
        }
        Ok(())
    }

    /// The Tokio handle, checked. The only way to obtain one.
    #[inline]
    pub(crate) fn enter(&self) -> SyncResult<tokio::runtime::Handle> {
        self.check()?;
        Ok(self.handle.clone())
    }
}

/// A resource that cannot be touched without passing the fork check.
///
/// The privacy is the point, not the wrapper: `inner` is unreachable from
/// `consumer.rs` except through [`Self::get`] or [`Self::enter`], which check
/// first, so the check stops being a call someone can forget. Used for a
/// `Reader` — the one thing a consumer touches that needs no runtime, and so
/// the one whose check is easiest to leave out.
pub(crate) struct Guarded<T> {
    rt: RuntimeRef,
    inner: T,
}

impl<T> Guarded<T> {
    pub(crate) fn new(rt: RuntimeRef, inner: T) -> Self {
        Self { rt, inner }
    }

    /// The value, checked. For work that needs no runtime.
    #[inline]
    pub(crate) fn get(&mut self) -> SyncResult<&mut T> {
        self.rt.check()?;
        Ok(&mut self.inner)
    }

    /// The value and a way to block, both checked. For work that waits.
    #[inline]
    pub(crate) fn enter(&mut self) -> SyncResult<(tokio::runtime::Handle, &mut T)> {
        let handle = self.rt.enter()?;
        Ok((handle, &mut self.inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::TokioAdapter;

    /// A `Runtime` over a real database, stamped `generations_behind` behind
    /// the process — one behind being what a forked child inherits.
    ///
    /// No thread, no `fork`: the point of moving the stamp onto a value. Proving
    /// a refusal used to mean forking from a parent holding a live Tokio
    /// runtime, the least safe moment there is, and failed 11 runs in 60.
    #[allow(clippy::type_complexity)]
    fn runtime_stamped(
        generations_behind: u64,
    ) -> ((tokio::runtime::Runtime, Arc<AimDb>), Runtime) {
        let tokio_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        let (db, _runner) = tokio_rt
            .block_on(async {
                AimDbBuilder::new()
                    .runtime(Arc::new(TokioAdapter))
                    .build()
                    .await
            })
            .expect("build database");

        // Kept alive by the returned guard: `Runtime` holds it weakly.
        let db = Arc::new(db);
        let mut runtime = Runtime::new(tokio_rt.handle().clone(), &db);
        runtime.made_in = crate::fork::generation().wrapping_sub(generations_behind);

        // Returned so the caller keeps both alive: the cloned handle must not
        // outlive its runtime, nor the database the `Runtime` pointing at it.
        ((tokio_rt, db), runtime)
    }

    #[test]
    fn a_runtime_from_this_generation_is_usable() {
        let (_guard, rt) = runtime_stamped(0);
        assert!(rt.check().is_ok());
        assert!(rt.enter().is_ok());
        assert!(rt.db().is_ok());
    }

    /// Every route in refuses — not just the one someone remembered to guard.
    /// This is the property the old per-object `check_fork` could not state.
    #[test]
    fn a_runtime_from_before_a_fork_refuses_every_route_in() {
        let (_guard, rt) = runtime_stamped(1);
        assert!(matches!(rt.check(), Err(SyncError::ForkedChild)));
        assert!(matches!(rt.enter(), Err(SyncError::ForkedChild)));
        assert!(matches!(rt.db(), Err(SyncError::ForkedChild)));
    }

    /// A child that forked twice is no more usable than one that forked once.
    #[test]
    fn any_distance_behind_refuses() {
        let (_guard, rt) = runtime_stamped(2);
        assert!(matches!(rt.check(), Err(SyncError::ForkedChild)));
    }

    /// Terminal for the same reason `RuntimeShutdown` is: the thread is gone
    /// and will not come back in this process, so a caller must not retry.
    #[test]
    fn the_refusal_classifies_as_closed() {
        use aimdb_core::DbErrorKind;
        let (_guard, rt) = runtime_stamped(1);
        let err = rt.enter().expect_err("must refuse");
        assert_eq!(err.kind(), DbErrorKind::Closed);
    }

    /// A view is not a way around [`Runtime::enter`].
    #[test]
    fn a_view_of_a_forked_runtime_cannot_be_taken() {
        let (_guard, rt) = runtime_stamped(1);
        assert!(matches!(rt.view(), Err(SyncError::ForkedChild)));
    }

    /// The detach case: the runtime is gone but this process dropped it, so the
    /// buffer must still drain — what the characterization tests pin.
    #[test]
    fn a_view_outliving_its_runtime_still_reads() {
        let (_guard, rt) = runtime_stamped(0);
        let view = rt.view().expect("view");
        drop(rt);

        assert!(view.check().is_ok());
        assert!(view.enter().is_ok());
    }

    /// The fork case, and the one a `Weak` upgrade got wrong: releasing an
    /// inherited handle is what a child is *supposed* to do, so a gone runtime
    /// says nothing about who owns the thread. This answered `Ok` before.
    #[test]
    fn a_view_outliving_its_runtime_in_a_forked_child_refuses() {
        let (_guard, rt) = runtime_stamped(0);
        let mut view = rt.view().expect("view");

        // Forked after the view was taken — the order that matters.
        view.made_in = crate::fork::generation().wrapping_sub(1);
        drop(rt);

        assert!(matches!(view.check(), Err(SyncError::ForkedChild)));
        assert!(matches!(view.enter(), Err(SyncError::ForkedChild)));
    }

    /// Both routes check, including `get`, which needs no runtime — `try_get`
    /// reads straight out of the buffer, so it cannot lean on `enter`.
    #[test]
    fn a_guarded_value_is_unreachable_after_a_fork() {
        let (_guard, rt) = runtime_stamped(0);
        let mut guarded = Guarded::new(rt.view().expect("view"), 7u32);

        assert_eq!(*guarded.get().expect("reads before a fork"), 7);
        assert!(guarded.enter().is_ok());

        guarded.rt.made_in = crate::fork::generation().wrapping_sub(1);

        assert!(matches!(guarded.get(), Err(SyncError::ForkedChild)));
        assert!(matches!(guarded.enter(), Err(SyncError::ForkedChild)));
    }
}
