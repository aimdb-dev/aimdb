//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities (time, sleep, logging)
//! as a concrete value wrapping `Arc<dyn RuntimeOps>` — no runtime type
//! parameter (issue #131, design doc 034 §3.2/§3.3).

use aimdb_executor::{BoxFuture, LogLevel, RuntimeOps};
use alloc::sync::Arc;

/// Unified runtime context for AimDB services
///
/// Wraps the dyn-safe [`RuntimeOps`] capability surface so services can use
/// timing and logging without knowing the runtime adapter type. Cheap to
/// clone (one `Arc`).
#[derive(Clone)]
pub struct RuntimeContext {
    ops: Arc<dyn RuntimeOps>,
}

impl RuntimeContext {
    /// Create a new RuntimeContext from the dyn-safe runtime capabilities.
    pub fn new(ops: Arc<dyn RuntimeOps>) -> Self {
        Self { ops }
    }

    /// Access time utilities
    ///
    /// Returns a time accessor for clock reads and sleeping.
    pub fn time(&self) -> Time<'_> {
        Time { ops: &*self.ops }
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
/// Durations are plain [`core::time::Duration`]; instants are `u64`
/// nanoseconds from an arbitrary monotonic epoch (only differences between
/// two readings are meaningful).
pub struct Time<'a> {
    ops: &'a dyn RuntimeOps,
}

impl Time<'_> {
    /// Monotonic clock reading in nanoseconds from an arbitrary epoch.
    ///
    /// Never decreases; saturates at `u64::MAX` rather than wrapping.
    pub fn now(&self) -> u64 {
        self.ops.now_nanos()
    }

    /// Wall-clock time as `(seconds, nanoseconds)` since the Unix epoch, if
    /// the runtime has a real-time clock (`None` on e.g. a bare MCU without
    /// an RTC anchor).
    pub fn unix_time(&self) -> Option<(u64, u32)> {
        self.ops.unix_time()
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
