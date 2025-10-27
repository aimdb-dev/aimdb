//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities like sleep and timestamp
//! functions, abstracting away the specific runtime adapter implementation.

use aimdb_executor::Runtime;
use core::future::Future;

/// Unified runtime context for AimDB services
///
/// Provides access to runtime capabilities (sleep, timestamps) through
/// a unified API, abstracting the underlying runtime implementation.
///
/// Services receive this context for timing operations without knowing
/// about the specific runtime adapter.
#[derive(Clone)]
pub struct RuntimeContext<R>
where
    R: Runtime,
{
    #[cfg(feature = "std")]
    runtime: std::sync::Arc<R>,
    #[cfg(not(feature = "std"))]
    runtime: &'static R,
}

#[cfg(feature = "std")]
impl<R> RuntimeContext<R>
where
    R: Runtime,
{
    /// Create a new RuntimeContext (std version uses Arc internally)
    pub fn new(runtime: R) -> Self {
        Self {
            runtime: std::sync::Arc::new(runtime),
        }
    }

    /// Create from an existing Arc to avoid double-wrapping
    pub fn from_arc(runtime: std::sync::Arc<R>) -> Self {
        Self { runtime }
    }
}

#[cfg(not(feature = "std"))]
impl<R> RuntimeContext<R>
where
    R: Runtime,
{
    /// Create a new RuntimeContext with static reference (no_std version)
    pub fn new(runtime: &'static R) -> Self {
        Self { runtime }
    }
}

impl<R> RuntimeContext<R>
where
    R: Runtime,
{
    /// Access time utilities
    ///
    /// Returns a time accessor for duration creation, sleep, and timing operations.
    pub fn time(&self) -> Time<'_, R> {
        Time { ctx: self }
    }

    /// Access logging utilities
    pub fn log(&self) -> Log<'_, R> {
        Log { ctx: self }
    }

    /// Get access to the underlying runtime
    ///
    /// This provides direct access to the runtime for advanced use cases.
    #[cfg(feature = "std")]
    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    #[cfg(not(feature = "std"))]
    pub fn runtime(&self) -> &'static R {
        self.runtime
    }
}

#[cfg(feature = "std")]
impl<R> RuntimeContext<R>
where
    R: Runtime,
{
    /// Create a RuntimeContext from a runtime adapter (std version with Arc)
    pub fn from_runtime(runtime: R) -> Self {
        Self::new(runtime)
    }
}

/// Create a RuntimeContext from any Runtime implementation (std version)
#[cfg(feature = "std")]
pub fn create_runtime_context<R>(runtime: R) -> RuntimeContext<R>
where
    R: Runtime,
{
    RuntimeContext::from_runtime(runtime)
}

/// Time utilities accessor for RuntimeContext
///
/// Provides duration creation, sleep, and timing measurements.
pub struct Time<'a, R: Runtime> {
    ctx: &'a RuntimeContext<R>,
}

impl<'a, R: Runtime> Time<'a, R> {
    /// Create a duration from milliseconds
    pub fn millis(&self, millis: u64) -> R::Duration {
        self.ctx.runtime.millis(millis)
    }

    /// Create a duration from seconds
    pub fn secs(&self, secs: u64) -> R::Duration {
        self.ctx.runtime.secs(secs)
    }

    /// Create a duration from microseconds
    pub fn micros(&self, micros: u64) -> R::Duration {
        self.ctx.runtime.micros(micros)
    }

    /// Sleep for the specified duration
    pub fn sleep(&self, duration: R::Duration) -> impl Future<Output = ()> + Send + '_ {
        self.ctx.runtime.sleep(duration)
    }

    /// Get the current timestamp
    pub fn now(&self) -> R::Instant {
        self.ctx.runtime.now()
    }

    /// Get the duration between two instants
    ///
    /// Returns None if `later` is before `earlier`.
    pub fn duration_since(&self, later: R::Instant, earlier: R::Instant) -> Option<R::Duration> {
        self.ctx.runtime.duration_since(later, earlier)
    }
}

/// Log utilities accessor for RuntimeContext
///
/// Provides structured logging operations.
pub struct Log<'a, R: Runtime> {
    ctx: &'a RuntimeContext<R>,
}

impl<'a, R: Runtime> Log<'a, R> {
    /// Log an informational message
    pub fn info(&self, message: &str) {
        self.ctx.runtime.info(message)
    }

    /// Log a debug message
    pub fn debug(&self, message: &str) {
        self.ctx.runtime.debug(message)
    }

    /// Log a warning message
    pub fn warn(&self, message: &str) {
        self.ctx.runtime.warn(message)
    }

    /// Log an error message
    pub fn error(&self, message: &str) {
        self.ctx.runtime.error(message)
    }
}
