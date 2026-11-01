//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities (time, sleep, logging)
//! as a concrete value wrapping `Arc<dyn RuntimeOps>` and the clock time is
//! read from — no runtime type parameter.

use crate::clock::{InstanceClock, Stamp};
use crate::executor::{BoxFuture, LogLevel, RuntimeOps};
use alloc::sync::Arc;

/// Unified runtime context for AimDB services
///
/// Wraps the dyn-safe [`RuntimeOps`] capability surface and a clock so
/// services can use timing and logging without knowing the runtime adapter
/// type. Cheap to clone (two `Arc`s).
///
/// Every context an `AimDb` hands out carries the instance clock, the one that
/// stamps writes (design 058 §3.1).
#[derive(Clone)]
pub struct RuntimeContext {
    ops: Arc<dyn RuntimeOps>,
    clock: Arc<dyn InstanceClock>,
}

impl RuntimeContext {
    /// A context with a clock of its own, created now — for code outside an
    /// instance, such as a standalone client engine. Inside an instance use
    /// `AimDb::runtime_ctx()`, which shares the instance clock.
    pub fn new(ops: Arc<dyn RuntimeOps>) -> Self {
        let clock = ops.new_clock();
        Self { ops, clock }
    }

    /// A context over `ops` that reads time from `clock`.
    pub fn with_clock(ops: Arc<dyn RuntimeOps>, clock: Arc<dyn InstanceClock>) -> Self {
        Self { ops, clock }
    }

    /// Access time utilities
    ///
    /// Returns a time accessor for clock reads and sleeping.
    pub fn time(&self) -> Time<'_> {
        Time {
            ops: &*self.ops,
            clock: &*self.clock,
        }
    }

    /// The clock this context reads time from.
    pub fn clock(&self) -> &Arc<dyn InstanceClock> {
        &self.clock
    }

    /// Access logging utilities
    pub fn log(&self) -> Log<'_> {
        Log { ops: &*self.ops }
    }

    /// Direct access to the underlying runtime capabilities.
    pub fn ops(&self) -> &Arc<dyn RuntimeOps> {
        &self.ops
    }
}

/// Time utilities accessor for RuntimeContext
///
/// Durations are plain [`core::time::Duration`]; instants are [`Stamp`]s
/// on the context's clock.
pub struct Time<'a> {
    ops: &'a dyn RuntimeOps,
    clock: &'a dyn InstanceClock,
}

impl Time<'_> {
    /// Monotonic reading of the context's clock: inside an instance, time
    /// since the instance started, on the clock that stamps writes.
    pub fn now(&self) -> Stamp {
        self.clock.now()
    }

    /// Wall-clock time as `(seconds, nanoseconds)` since the Unix epoch, if
    /// the runtime has a real-time clock (`None` on e.g. a bare MCU without
    /// an RTC anchor). May be adjusted; unrelated to [`now`](Self::now).
    pub fn unix_time(&self) -> Option<(u64, u32)> {
        self.clock.unix_time()
    }

    /// Completes after at least `duration` has elapsed.
    ///
    /// The future is boxed (it crosses the `dyn RuntimeOps` boundary), so each
    /// call costs one small heap allocation. That is fine for periodic loops
    /// and timeouts; in a no_std hot path that sleeps every iteration, await
    /// the adapter's native timer (e.g. `embassy_time::Timer`) directly
    /// instead.
    pub fn sleep(&self, duration: core::time::Duration) -> BoxFuture {
        self.ops.sleep(duration)
    }

    /// Convenience: sleep for `ms` milliseconds.
    pub fn sleep_millis(&self, ms: u64) -> BoxFuture {
        self.ops.sleep(core::time::Duration::from_millis(ms))
    }

    /// Convenience: sleep for `secs` seconds.
    pub fn sleep_secs(&self, secs: u64) -> BoxFuture {
        self.ops.sleep(core::time::Duration::from_secs(secs))
    }
}

/// Log utilities accessor for RuntimeContext
///
/// Provides structured logging operations.
pub struct Log<'a> {
    ops: &'a dyn RuntimeOps,
}

impl Log<'_> {
    /// Log an informational message
    pub fn info(&self, message: &str) {
        self.ops.log(LogLevel::Info, message)
    }

    /// Log a debug message
    pub fn debug(&self, message: &str) {
        self.ops.log(LogLevel::Debug, message)
    }

    /// Log a warning message
    pub fn warn(&self, message: &str) {
        self.ops.log(LogLevel::Warn, message)
    }

    /// Log an error message
    pub fn error(&self, message: &str) {
        self.ops.log(LogLevel::Error, message)
    }
}
