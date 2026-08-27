//! Making a `fork()` visible to handles created before it.
//!
//! `fork` copies the address space but not the threads, so a child inherits
//! every [`AimDbHandle`](crate::AimDbHandle), [`SyncProducer`](crate::SyncProducer)
//! and [`SyncConsumer`](crate::SyncConsumer) the parent held — and none of the
//! runtime thread that makes them work. Without the check below, the child's
//! `set()` pushes into a buffer nobody drains and returns `Ok`.
//!
//! # Why a generation counter and not a flag
//!
//! A `bool` would poison the child permanently, including for a database the
//! *child itself* attaches afterwards. A counter makes staleness relative: a
//! handle is unusable when the process has forked since it was made, and one
//! made after the fork is fine.
//!
//! # Why `pthread_atfork` and not `getpid`
//!
//! Because this sits on the publish path. Measured: `try_set` is 121 ns and
//! `std::process::id()` is 321 ns, so reading the pid per call would cost more
//! than twice the work it guards. A relaxed atomic load does not measurably
//! cost anything.
//!
//! # The process-global caveat
//!
//! Registering a `pthread_atfork` handler is a process-wide decision, and a
//! library making one on an application's behalf is normally a trespass. This
//! is the exception: only the crate that owns the runtime thread knows the
//! thread is gone, so nobody above can make this check — and the handler is
//! registered lazily, on the first [`generation`] (which every `attach` takes),
//! so a program that never uses the sync facade never gets one.

use core::sync::atomic::{AtomicU64, Ordering};

/// How many times this process has forked, as observed by the child. Parents
/// never see it change.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The generation a handle, producer or consumer was created in.
///
/// Compared, never interpreted: only equality with [`generation`] means
/// anything.
pub(crate) type Generation = u64;

#[cfg(unix)]
extern "C" fn on_fork_in_child() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Register the fork handler, once per process.
///
/// Called from the constructors and from [`generation`] rather than a static
/// initialiser, so a program that never uses the sync facade never installs a
/// handler. Idempotent and cheap after the first call — a completed
/// [`Once`](std::sync::Once) is one acquire load — which is why [`generation`]
/// can afford to call it and [`forked_since`] does not have to.
pub(crate) fn arm() {
    #[cfg(unix)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: `on_fork_in_child` is `extern "C"`, does not unwind, and
            // performs one relaxed atomic add — permitted in a fork handler.
            unsafe {
                libc::pthread_atfork(None, None, Some(on_fork_in_child));
            }
        });
    }
}

/// Read the counter. Never arms — see [`generation`] for why that split
/// exists.
#[inline]
fn load() -> Generation {
    GENERATION.load(Ordering::Relaxed)
}

/// The generation to stamp on something being created now.
///
/// Crate-private, and staying that way. A layer built *on* this crate has the
/// same problem, and is served by
/// [`SyncProducer::check`](crate::SyncProducer::check) — the question it
/// actually has ("can I still publish?"), rather than the stamp-and-compare
/// mechanism, which publishing this pair would pin us to in semver.
///
/// **Arms the handler**, because otherwise it hands out a number that cannot
/// change. A caller above this crate stamps its own state before any database
/// is attached — that is the normal order, an FFI door opens before it is used
/// — and until the first `attach` there is no handler, so a `fork` in that
/// window would go uncounted and the stamp would compare equal forever. That is
/// the very bug this module exists to prevent, one layer up. Arming here closes
/// it: taking a stamp is what makes the stamp meaningful.
///
/// This is the *construction-time* call and is cold. The hot path is
/// [`forked_since`], which only loads.
pub(crate) fn generation() -> Generation {
    arm();
    load()
}

/// Whether this process has forked since `made_in` was taken from
/// [`generation`].
///
/// One relaxed load and a comparison — no syscall, no lock, and no arming: if
/// `made_in` exists then [`generation`] already armed the handler. Safe to call
/// from anywhere, including while the runtime thread is mid-shutdown.
pub(crate) fn forked_since(made_in: Generation) -> bool {
    load() != made_in
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A stamp taken before any `attach` must still be invalidated by a fork.
    ///
    /// The handler is armed lazily, so something has to trigger it. If arming
    /// were left to the first `attach`, a stamp taken earlier would compare
    /// equal forever and a `fork` in that window would go uncounted — the
    /// silent-success failure this module exists to prevent, one layer up.
    ///
    /// This is a unit test rather than an integration test for two reasons.
    /// [`generation`] is crate-private. And the precondition is that *nothing*
    /// in the process has armed the handler yet: the lib test binary holds only
    /// the compile-time `assert_send`/`assert_sync` checks and the
    /// `SyncError::kind` tests, none of which attach a database, whereas any
    /// binary sharing space with the fork suite would be armed by its first
    /// `attach` and this test would then assert nothing.
    #[test]
    fn a_stamp_taken_before_any_attach_still_sees_a_fork() {
        let before_any_attach = generation();
        assert!(!forked_since(before_any_attach), "nothing has forked yet");

        // SAFETY: the child reads one atomic and `_exit`s. It never allocates,
        // so the usual "child deadlocks on an allocator lock inherited from a
        // thread that did not survive the fork" hazard does not arise.
        let pid = unsafe { libc::fork() };
        assert_ne!(pid, -1, "fork failed");

        if pid == 0 {
            let saw_it = forked_since(before_any_attach);
            // SAFETY: ends the child without running destructors, by design.
            unsafe { libc::_exit(if saw_it { 0 } else { 1 }) }
        }

        let status = wait_briefly(pid);
        assert!(libc::WIFEXITED(status), "child did not exit normally");
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "a stamp taken before the first attach must still see the fork"
        );

        // Only the child's counter moved; the parent is never poisoned.
        assert!(
            !forked_since(before_any_attach),
            "the parent must not be poisoned by forking a child"
        );
    }

    /// Reap `pid`, but fail rather than block forever.
    ///
    /// The child above cannot deadlock, but "cannot" is what was said about the
    /// fork suite before it hung a CI job for six hours. A bounded wait costs
    /// nothing and keeps that failure mode impossible here by construction.
    fn wait_briefly(pid: libc::pid_t) -> libc::c_int {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: `pid` is our child and `status` is a valid out-pointer.
            let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if waited == pid {
                return status;
            }
            assert_eq!(waited, 0, "waitpid failed");
            if Instant::now() >= deadline {
                // SAFETY: `pid` is our child; SIGKILL cannot be blocked.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    let mut discard: libc::c_int = 0;
                    libc::waitpid(pid, &mut discard, 0);
                }
                panic!("forked child did not finish within 30s");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
