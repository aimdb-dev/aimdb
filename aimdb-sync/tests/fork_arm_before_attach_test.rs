//! A stamp taken before the first `attach` must still see a `fork()`.
//!
//! The `pthread_atfork` handler is registered lazily, so something has to
//! trigger it. Handles arm it on `attach`, but [`aimdb_sync::fork::generation`]
//! is public precisely so a layer *above* this crate can stamp state of its own
//! — an FFI door opens before any database is attached, which is the normal
//! order. If arming were left to the first `attach`, a `fork` in that window
//! would go uncounted and the stamp would compare equal forever: the
//! silent-success failure this module exists to prevent, one layer up.
//!
//! This lives in its own test binary on purpose. Any `attach` anywhere in the
//! process arms the handler, so a test sharing a binary with the rest of the
//! fork suite would pass whether or not `generation()` arms — it would assert
//! nothing.
#![cfg(all(unix, feature = "std"))]

/// Taken before anything else in this process. No database is ever attached
/// here, so `generation()` is the only thing that can arm the handler.
#[test]
fn a_stamp_taken_before_any_attach_still_sees_a_fork() {
    let before_any_attach = aimdb_sync::fork::generation();
    assert!(
        !aimdb_sync::fork::forked_since(before_any_attach),
        "nothing has forked yet"
    );

    // SAFETY: the child only reads an atomic and `_exit`s — no allocation, so
    // the usual fork-in-a-multi-threaded-parent hazard does not apply.
    match unsafe { libc::fork() } {
        -1 => panic!("fork failed"),
        0 => {
            let saw_it = aimdb_sync::fork::forked_since(before_any_attach);
            unsafe { libc::_exit(if saw_it { 0 } else { 1 }) }
        }
        pid => {
            let mut status: libc::c_int = 0;
            // SAFETY: `pid` is our child and `status` is a valid out-pointer.
            let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
            assert_eq!(waited, pid, "waitpid");
            assert!(libc::WIFEXITED(status), "child did not exit normally");
            assert_eq!(
                libc::WEXITSTATUS(status),
                0,
                "a stamp taken before the first attach must still see the fork"
            );

            // The parent's own stamp is untouched: only the child's counter moved.
            assert!(
                !aimdb_sync::fork::forked_since(before_any_attach),
                "the parent must not be poisoned by forking a child"
            );
        }
    }
}
