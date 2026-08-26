//! Running a closure in a `fork()`ed child, with a bound on how long it gets.
//!
//! # Why the bound exists
//!
//! `fork` in a multi-threaded process gives the child an address space in which
//! every lock held by a thread that did not come across is held forever — the
//! allocator's above all. Strictly, only async-signal-safe work is sound in the
//! child, and `malloc`/`free` are not.
//!
//! This is not theoretical. The first CI run of this suite deadlocked and sat
//! until GitHub's six-hour job ceiling killed it, taking the dependent jobs
//! with it.
//!
//! # What actually causes it, measured
//!
//! Running one binary 60 times per variant, counting hangs:
//!
//! | variant | hangs |
//! |---|---|
//! | as written, tests in parallel | 11/60 |
//! | as written, `--test-threads=1` | 17/60 |
//! | children leak instead of freeing | 1/60 |
//! | children leak where they can | 11/120 |
//! | children leak where they can, plus [`settle`] | **0/120** |
//!
//! Two things follow. Harness parallelism is *not* the driver — serialising the
//! tests made it no better, so do not reach for `--test-threads=1`. What does
//! matter is the allocator lock: the child takes it on every `free`, and the
//! parent's own freshly-spawned runtime threads are the ones holding it.
//!
//! So both ends are addressed. The children below `mem::forget` their inherited
//! state wherever the destructor is not what is under test — they `_exit`
//! immediately, so nothing is lost — and [`settle`] gives the parent time to go
//! quiet before the fork. Alone, the first halves the rate; together they take
//! it to zero over 120 runs.
//!
//! The watchdog is still not optional. `dropping_an_inherited_handle_does_not_
//! panic` exists to run the destructor and so must free, and [`settle`] is a
//! duration rather than a handshake, so neither guarantee is absolute. A test
//! that can hang must not be able to hang *quietly*: [`in_forked_child`] kills
//! the child and fails once [`CHILD_TIMEOUT`] passes, so the worst case is a
//! fast, legible failure rather than a burnt runner.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// How long a child gets before it is treated as deadlocked.
///
/// Not a tuning knob for slow machines: the work each child does is
/// sub-millisecond, so this is the line past which the child is stuck rather
/// than slow. Generous enough that a loaded runner is never mistaken for a
/// deadlock.
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

const POLL: Duration = Duration::from_millis(5);

/// How long to let the parent go quiet before forking. See [`settle`].
const SETTLE: Duration = Duration::from_millis(100);

/// Wait for the parent's runtime threads to finish starting and park.
///
/// `fork` is only safe from a quiescent parent, and a test that has just called
/// `attach` is the opposite of one: the runtime thread and its Tokio workers are
/// mid-startup and allocating hard, so a fork caught in that window hands the
/// child an allocator lock held by a thread that does not exist on its side.
///
/// A sleep in a test is usually a smell. Here it is the precondition itself,
/// and it is the difference between a suite that hangs and one that does not:
///
/// | variant | hangs |
/// |---|---|
/// | children free, no settle | 11/60 |
/// | children leak where they can, no settle | 11/120 |
/// | children leak where they can, **with settle** | **0/120** |
///
/// There is no way to observe "the runtime has parked" from out here, so this
/// is a duration rather than a handshake — which is why the watchdog stays: if
/// the wait is ever too short on a slower machine, the result is a bounded
/// failure and not a six-hour job.
fn settle() {
    std::thread::sleep(SETTLE);
}

/// Run `child` in a forked child and return its exit code.
///
/// The child ends with `_exit`, which skips both the test harness's cleanup and
/// every destructor — the child is not a test runner, and letting it unwind
/// would report a second set of results into the parent's stdout.
///
/// Panics if the child does not finish within [`CHILD_TIMEOUT`], after killing
/// it. See the module note for why that matters.
pub fn in_forked_child(child: impl FnOnce() -> bool) -> i32 {
    settle();

    // SAFETY: `fork` itself is safe to call here; what the child may then do is
    // the real constraint, and the module note covers it.
    match unsafe { libc::fork() } {
        -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
        0 => {
            let ok = child();
            // SAFETY: ends the child without running destructors, by design.
            unsafe { libc::_exit(if ok { 0 } else { 1 }) }
        }
        pid => reap_within_timeout(pid),
    }
}

/// Wait for `pid`, but never longer than [`CHILD_TIMEOUT`].
fn reap_within_timeout(pid: libc::pid_t) -> i32 {
    let deadline = Instant::now() + CHILD_TIMEOUT;

    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `pid` is our child and `status` is a valid out-pointer.
        // `WNOHANG` makes this a poll rather than a block, which is the whole
        // point — a blocking wait is what turned a deadlock into a six-hour job.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };

        if waited == pid {
            assert!(
                libc::WIFEXITED(status),
                "child did not exit normally — a panic or signal, status {status}"
            );
            return libc::WEXITSTATUS(status);
        }

        assert!(
            waited == 0,
            "waitpid failed: {}",
            std::io::Error::last_os_error()
        );

        if Instant::now() >= deadline {
            kill_and_reap(pid);
            panic!(
                "forked child {pid} did not finish within {CHILD_TIMEOUT:?}, so it is \
                 deadlocked rather than slow — most likely on a lock it inherited held \
                 from a thread that did not survive the fork. Killed it rather than \
                 letting the job hang."
            );
        }

        std::thread::sleep(POLL);
    }
}

/// Kill a stuck child and reap it, so the run does not leave a zombie behind.
fn kill_and_reap(pid: libc::pid_t) {
    // SAFETY: `pid` is our own child, and SIGKILL cannot be caught or blocked,
    // so the blocking wait below cannot itself hang.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status: libc::c_int = 0;
        libc::waitpid(pid, &mut status, 0);
    }
}
