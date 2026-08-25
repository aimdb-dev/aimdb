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
//! registered lazily, on the first `attach`, so a program that never uses the
//! sync facade never gets one.

use core::sync::atomic::{AtomicU64, Ordering};

/// How many times this process has forked, as observed by the child. Parents
/// never see it change.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The generation a handle, producer or consumer was created in.
///
/// Compared, never interpreted: only equality with [`generation`] means
/// anything.
pub type Generation = u64;

#[cfg(unix)]
extern "C" fn on_fork_in_child() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Register the fork handler, once per process.
///
/// Called from the constructors rather than a static initialiser, so a program
/// that never attaches a database never installs a handler.
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

/// The generation to stamp on something being created now.
///
/// Public because a layer built *on* this crate has the same problem: an FFI
/// door holds state of its own that a `fork` invalidates, and needs to answer
/// "is this still usable" without locking anything the runtime thread might
/// hold. Record this at construction, compare with [`forked_since`].
pub fn generation() -> Generation {
    GENERATION.load(Ordering::Relaxed)
}

/// Whether this process has forked since `made_in` was taken from
/// [`generation`].
///
/// One relaxed load and a comparison — no syscall, no lock. Safe to call from
/// anywhere, including while the runtime thread is mid-shutdown.
pub fn forked_since(made_in: Generation) -> bool {
    generation() != made_in
}
