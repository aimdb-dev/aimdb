//! The runtime thread, as one value.
//!
//! `aimdb-sync` spawns an OS thread to own a Tokio runtime on the caller's
//! behalf. Everything that thread *is* — the way in, the database it built, and
//! the `fork` generation it belongs to — lives here, in one type, reached by
//! every object that needs it.
//!
//! # Why this is one type
//!
//! It used to be four fields on [`AimDbHandle`](crate::AimDbHandle) and a fork
//! stamp copied into three others. That shape cost four bugs in one review
//! pass, all of the same kind: someone had to remember something and the
//! compiler could not help. A guard was added to a new method — or wasn't. A
//! field was added to the handle — and the release path forgot it.
//!
//! # Why the check lives on the way in
//!
//! [`Runtime::enter`] is the only route to the Tokio handle and
//! [`Runtime::db`] the only route to the database, and both refuse a runtime
//! this process did not inherit a thread for. A publish or a read cannot be
//! written that skips the check, because it cannot be written without going
//! through one of them. Design 050 calls this "checked by construction rather
//! than by convention"; #231 gave panic-freedom the same treatment.
//!
//! The check is deliberately *before* the database is handed out. A forked
//! child's `Arc<AimDb>` is perfectly valid — it came across with the address
//! space — so a child that reached the database would publish into a buffer
//! nobody drains and be told `Ok`. That silence is the bug this whole mechanism
//! exists to prevent.
//!
//! # Who owns it
//!
//! The handle owns the `Arc<Runtime>`; producers and consumers hold a
//! [`Weak`](alloc::sync::Weak). So the database dies with the handle, exactly
//! as it did when producers held `Weak<AimDb>` directly, and two things stay
//! free that shared ownership would have made expensive:
//!
//! - **Liveness.** A failed upgrade *is* the check — no flag to keep in sync.
//! - **Waking a blocked reader.** Dropping the database closes its buffers,
//!   which is what wakes a consumer parked in `get()`. `aimdb-core` has no
//!   explicit close, so nothing else would.
//!
//! An `Arc` here was tried and reverted. It bought one thing — a producer
//! outliving its handle keeps working — and cost both of the above, each of
//! which had to be rebuilt by hand (a liveness flag, a level-triggered stop
//! channel, and a select around every blocking read). It also made a forgotten
//! producer keep an OS thread and a Tokio runtime alive with nobody owning
//! them, which is the stranded thread #232 had just removed.

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
    db: Arc<AimDb>,

    /// The fork generation this runtime's thread was spawned in.
    ///
    /// Plain data, which is the point: every refusal path can be tested by
    /// building a `Runtime` with a stale value, without a thread and without a
    /// real `fork`. See the unit tests below.
    made_in: crate::fork::Generation,
}

impl Runtime {
    pub(crate) fn new(handle: tokio::runtime::Handle, db: Arc<AimDb>) -> Self {
        Self {
            handle,
            db,
            made_in: crate::fork::generation(),
        }
    }

    /// Whether this process still owns the thread this runtime names.
    ///
    /// One relaxed atomic load and a comparison — no syscall, no lock. It sits
    /// on the publish path, which is why it is not a `getpid` call.
    #[inline]
    pub(crate) fn check(&self) -> SyncResult<()> {
        if crate::fork::forked_since(self.made_in) {
            return Err(SyncError::ForkedChild);
        }
        Ok(())
    }

    /// The Tokio handle, or [`SyncError::ForkedChild`].
    ///
    /// The only way to reach it. Blocking on a runtime whose thread did not
    /// survive a `fork` would park forever.
    #[inline]
    pub(crate) fn enter(&self) -> SyncResult<&tokio::runtime::Handle> {
        self.check()?;
        Ok(&self.handle)
    }

    /// The database, or [`SyncError::ForkedChild`].
    ///
    /// The only way to reach it. See the module note on why the check must come
    /// first: a forked child's handle to the database is valid, and that is
    /// exactly the problem.
    #[inline]
    pub(crate) fn db(&self) -> SyncResult<&Arc<AimDb>> {
        self.check()?;
        Ok(&self.db)
    }

    /// The database without the fork check, for the one caller that has already
    /// made it: [`AimDbHandle::consumer`](crate::AimDbHandle::consumer) checks,
    /// then subscribes.
    ///
    /// Not a hole in the guarantee — it is `pub(crate)` and its one use sits
    /// directly after a [`Self::check`] — but it is the reason [`Self::db`]
    /// exists as the ordinary path.
    #[inline]
    pub(crate) fn db_unchecked(&self) -> &Arc<AimDb> {
        &self.db
    }
}

/// A borrowed view of a [`Runtime`] that outlives it on purpose.
///
/// Held by [`SyncConsumer`](crate::SyncConsumer), which has a requirement the
/// handle and the producers do not: a `Reader` can still drain what is already
/// buffered after the runtime is gone, and delivering that data is behaviour
/// the characterization tests pin. So a dead runtime must not refuse a read —
/// but a `fork` still must.
///
/// The Tokio handle is kept for that case and is **private to this module**,
/// which is the whole point of the type. `consumer.rs` cannot reach it except
/// through [`Self::enter`], so a read that skips the check cannot be written
/// there any more than it can anywhere else. A bare handle field beside a
/// hand-written guard — the shape this replaces — offered no such thing.
pub(crate) struct RuntimeRef {
    rt: Weak<Runtime>,
    handle: tokio::runtime::Handle,
}

impl RuntimeRef {
    pub(crate) fn new(rt: Weak<Runtime>, handle: tokio::runtime::Handle) -> Self {
        Self { rt, handle }
    }

    /// Refuse a forked child; let a detached one through.
    ///
    /// A failed upgrade means the handle was dropped in *this* process, which
    /// is not a fork — the buffer is the right thing to answer for itself.
    /// After a real `fork` the upgrade **succeeds**, because the `Arc` came
    /// across with the address space, which is exactly why the check is worth
    /// making at all.
    #[inline]
    pub(crate) fn check(&self) -> SyncResult<()> {
        match self.rt.upgrade() {
            Some(rt) => rt.check(),
            None => Ok(()),
        }
    }

    /// The Tokio handle, checked. The only way to obtain one.
    #[inline]
    pub(crate) fn enter(&self) -> SyncResult<tokio::runtime::Handle> {
        self.check()?;
        Ok(self.handle.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::TokioAdapter;

    /// A `Runtime` over a real database, stamped `generations_behind` behind
    /// the process. One behind is what a forked child's inherited runtime
    /// looks like.
    ///
    /// No thread is spawned and no `fork` happens — which is the point of
    /// moving the stamp onto a value. Before this, proving a refusal path meant
    /// forking a real process from a parent holding a live Tokio runtime, the
    /// least safe moment there is; that suite failed 11 runs in 60 until it was
    /// mitigated (design 050 §8).
    fn runtime_stamped(generations_behind: u64) -> (tokio::runtime::Runtime, Runtime) {
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

        let mut runtime = Runtime::new(tokio_rt.handle().clone(), Arc::new(db));
        runtime.made_in = crate::fork::generation().wrapping_sub(generations_behind);

        // Returned so the caller keeps it alive: the handle we cloned out of it
        // must not outlive the runtime it came from.
        (tokio_rt, runtime)
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
}
