//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities like sleep and timestamp
//! functions, abstracting away the specific runtime adapter implementation.

use aimdb_executor::Runtime;
use core::future::Future;
use core::time::Duration;

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
    /// Sleep for the specified duration
    ///
    /// This method delegates to the underlying runtime's sleep implementation,
    /// allowing services to sleep without knowing the specific runtime type.
    ///
    /// # Arguments
    ///
    /// * `duration` - How long to sleep
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_core::RuntimeContext;
    /// # use aimdb_tokio_adapter::TokioAdapter;
    /// # use std::time::Duration;
    /// # async fn example(ctx: &RuntimeContext<TokioAdapter>) {
    /// // In a service function
    /// ctx.sleep(Duration::from_millis(500)).await;
    /// # }
    /// ```
    pub fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send + '_ {
        self.runtime.sleep(duration)
    }

    /// Get the current timestamp
    ///
    /// This method delegates to the underlying runtime's timestamp implementation,
    /// providing a consistent way to get time information across different runtimes.
    ///
    /// # Returns
    ///
    /// Current instant/timestamp from the runtime
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_core::RuntimeContext;
    /// # use aimdb_tokio_adapter::TokioAdapter;
    /// # fn example(ctx: &RuntimeContext<TokioAdapter>) {
    /// let start_time = ctx.now();
    /// // ... do some work ...
    /// let end_time = ctx.now();
    /// # }
    /// ```
    pub fn now(&self) -> R::Instant {
        self.runtime.now()
    }

    /// Get the duration between two instants
    ///
    /// # Arguments
    ///
    /// * `later` - The later instant
    /// * `earlier` - The earlier instant
    ///
    /// # Returns
    ///
    /// Some(Duration) if later >= earlier, None otherwise
    pub fn duration_since(&self, later: R::Instant, earlier: R::Instant) -> Option<Duration> {
        self.runtime.duration_since(later, earlier)
    }

    /// Log an informational message
    ///
    /// This method delegates to the underlying runtime's logging implementation,
    /// providing a consistent way to log across different runtimes.
    ///
    /// # Arguments
    ///
    /// * `message` - The message to log
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_core::RuntimeContext;
    /// # use aimdb_tokio_adapter::TokioAdapter;
    /// # fn example(ctx: &RuntimeContext<TokioAdapter>) {
    /// ctx.info("Service started successfully");
    /// # }
    /// ```
    pub fn info(&self, message: &str) {
        self.runtime.info(message)
    }

    /// Log a debug message
    ///
    /// # Arguments
    ///
    /// * `message` - The message to log
    pub fn debug(&self, message: &str) {
        self.runtime.debug(message)
    }

    /// Log a warning message
    ///
    /// # Arguments
    ///
    /// * `message` - The message to log
    pub fn warn(&self, message: &str) {
        self.runtime.warn(message)
    }

    /// Log an error message
    ///
    /// # Arguments
    ///
    /// * `message` - The message to log
    pub fn error(&self, message: &str) {
        self.runtime.error(message)
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
