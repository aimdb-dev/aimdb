//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities like sleep and timestamp
//! functions, abstracting away the specific runtime adapter implementation.

use aimdb_executor::Runtime;
use core::future::Future;

/// Unified runtime context for AimDB services
///
/// This context provides access to essential runtime capabilities through
/// a clean, unified API. Services receive this context and can use it for
/// timing operations without needing to know about the underlying runtime.
///
/// The context holds a reference or smart pointer to the runtime, enabling
/// efficient sharing across service instances without requiring cloning.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::{RuntimeContext, service, DbResult};
/// use aimdb_tokio_adapter::TokioAdapter;
/// use std::time::Duration;
///
/// #[service]
/// async fn my_service(ctx: RuntimeContext<TokioAdapter>) -> DbResult<()> {
///     println!("Service starting at: {:?}", ctx.now());
///     
///     // Sleep using the runtime's sleep capability
///     ctx.sleep(Duration::from_millis(100)).await;
///     
///     println!("Service completed at: {:?}", ctx.now());
///     Ok(())
/// }
/// ```
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
    /// Create a new RuntimeContext with the given runtime adapter (std version)
    ///
    /// In std environments, the runtime is wrapped in an Arc for efficient sharing.
    ///
    /// # Arguments
    ///
    /// * `runtime` - Runtime adapter implementing the Runtime trait
    pub fn new(runtime: R) -> Self {
        Self {
            runtime: std::sync::Arc::new(runtime),
        }
    }

    /// Create a RuntimeContext from an Arc (for efficiency)
    ///
    /// This avoids double-wrapping when you already have an Arc.
    pub fn from_arc(runtime: std::sync::Arc<R>) -> Self {
        Self { runtime }
    }
}

#[cfg(not(feature = "std"))]
impl<R> RuntimeContext<R>
where
    R: Runtime,
{
    /// Create a new RuntimeContext with a static reference (no_std version)
    ///
    /// In no_std environments, requires a static reference since we can't use Arc.
    ///
    /// # Arguments
    ///
    /// * `runtime` - Static reference to runtime adapter
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
    /// Returns a time accessor that provides duration creation, sleep, and timing operations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_core::RuntimeContext;
    /// # use aimdb_tokio_adapter::TokioAdapter;
    /// # async fn example(ctx: &RuntimeContext<TokioAdapter>) {
    /// // Get time accessor
    /// let time = ctx.time();
    ///
    /// // Use it for various operations
    /// let start = time.now();
    /// time.sleep(time.millis(500)).await;
    /// let elapsed = time.duration_since(time.now(), start);
    /// # }
    /// ```
    pub fn time(&self) -> Time<'_, R> {
        Time { ctx: self }
    }

    /// Access logging utilities
    ///
    /// Returns a logger accessor that provides structured logging operations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_core::RuntimeContext;
    /// # use aimdb_tokio_adapter::TokioAdapter;
    /// # fn example(ctx: &RuntimeContext<TokioAdapter>) {
    /// ctx.log().info("Service started");
    /// ctx.log().warn("High memory usage");
    /// ctx.log().error("Connection failed");
    /// # }
    /// ```
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
    ///
    /// This is a convenience method that wraps the runtime in an Arc.
    ///
    /// # Arguments
    ///
    /// * `runtime` - Runtime adapter implementing the Runtime trait
    pub fn from_runtime(runtime: R) -> Self {
        Self::new(runtime)
    }
}

/// Create a RuntimeContext from any type that implements the Runtime trait (std version)
///
/// This function provides a generic way to create a RuntimeContext from any
/// runtime adapter, as long as it implements the Runtime trait.
/// This is particularly useful in macros where we don't know the concrete type.
///
/// # Arguments
/// * `runtime` - Any type implementing Runtime
///
/// # Returns
/// A RuntimeContext wrapping the provided runtime in an Arc
#[cfg(feature = "std")]
pub fn create_runtime_context<R>(runtime: R) -> RuntimeContext<R>
where
    R: Runtime,
{
    RuntimeContext::from_runtime(runtime)
}

/// Time utilities accessor for RuntimeContext
///
/// This accessor provides all time-related operations including duration creation,
/// sleep, and timing measurements. It encapsulates time functionality separately
/// from other context capabilities like logging.
///
/// # Design Philosophy
///
/// - **Separation of Concerns**: Time operations are isolated from logging and other capabilities
/// - **Clear Intent**: `ctx.time().sleep()` clearly indicates time-based operation
/// - **Extensible**: Easy to add new time-related methods without cluttering RuntimeContext
///
/// # Example
///
/// ```rust,ignore
/// async fn my_service<R: Runtime>(ctx: RuntimeContext<R>) {
///     let time = ctx.time();
///     
///     let start = time.now();
///     time.sleep(time.millis(100)).await;
///     let elapsed = time.duration_since(time.now(), start);
/// }
/// ```
pub struct Time<'a, R: Runtime> {
    ctx: &'a RuntimeContext<R>,
}

impl<'a, R: Runtime> Time<'a, R> {
    /// Create a duration from milliseconds
    ///
    /// # Example
    /// ```rust,ignore
    /// time.sleep(time.millis(500)).await;
    /// ```
    pub fn millis(&self, millis: u64) -> R::Duration {
        self.ctx.runtime.millis(millis)
    }

    /// Create a duration from seconds
    ///
    /// # Example
    /// ```rust,ignore
    /// time.sleep(time.secs(2)).await;
    /// ```
    pub fn secs(&self, secs: u64) -> R::Duration {
        self.ctx.runtime.secs(secs)
    }

    /// Create a duration from microseconds
    ///
    /// # Example
    /// ```rust,ignore
    /// time.sleep(time.micros(1000)).await;
    /// ```
    pub fn micros(&self, micros: u64) -> R::Duration {
        self.ctx.runtime.micros(micros)
    }

    /// Sleep for the specified duration
    ///
    /// # Example
    /// ```rust,ignore
    /// time.sleep(time.millis(100)).await;
    /// ```
    pub fn sleep(&self, duration: R::Duration) -> impl Future<Output = ()> + Send + '_ {
        self.ctx.runtime.sleep(duration)
    }

    /// Get the current timestamp
    ///
    /// # Example
    /// ```rust,ignore
    /// let now = time.now();
    /// ```
    pub fn now(&self) -> R::Instant {
        self.ctx.runtime.now()
    }

    /// Get the duration between two instants
    ///
    /// Returns None if `later` is before `earlier`
    ///
    /// # Example
    /// ```rust,ignore
    /// let start = time.now();
    /// // ... do work ...
    /// let end = time.now();
    /// let elapsed = time.duration_since(end, start);
    /// ```
    pub fn duration_since(&self, later: R::Instant, earlier: R::Instant) -> Option<R::Duration> {
        self.ctx.runtime.duration_since(later, earlier)
    }
}

/// Logging accessor for RuntimeContext
///
/// This accessor provides all logging operations, separating them from time and
/// other context capabilities for better organization and testability.
///
/// # Design Philosophy
///
/// - **Separation of Concerns**: Logging is isolated from time and other operations
/// - **Clear Intent**: `ctx.log().info()` clearly indicates logging operation
/// - **Mockable**: Easy to mock logging separately from other capabilities
///
/// # Example
///
/// ```rust,ignore
/// async fn my_service<R: Runtime>(ctx: RuntimeContext<R>) {
///     let log = ctx.log();
///     
///     log.info("Service starting");
///     log.debug("Debug information");
///     log.warn("Warning message");
///     log.error("Error occurred");
/// }
/// ```
pub struct Log<'a, R: Runtime> {
    ctx: &'a RuntimeContext<R>,
}

impl<'a, R: Runtime> Log<'a, R> {
    /// Log an informational message
    ///
    /// # Example
    /// ```rust,ignore
    /// ctx.log().info("Service started");
    /// ```
    pub fn info(&self, message: &str) {
        self.ctx.runtime.info(message)
    }

    /// Log a debug message
    ///
    /// # Example
    /// ```rust,ignore
    /// ctx.log().debug("Processing item");
    /// ```
    pub fn debug(&self, message: &str) {
        self.ctx.runtime.debug(message)
    }

    /// Log a warning message
    ///
    /// # Example
    /// ```rust,ignore
    /// ctx.log().warn("High memory usage");
    /// ```
    pub fn warn(&self, message: &str) {
        self.ctx.runtime.warn(message)
    }

    /// Log an error message
    ///
    /// # Example
    /// ```rust,ignore
    /// ctx.log().error("Connection failed");
    /// ```
    pub fn error(&self, message: &str) {
        self.ctx.runtime.error(message)
    }
}
