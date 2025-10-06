//! Runtime context for AimDB services
//!
//! Provides a unified interface to runtime capabilities like sleep and timestamp
//! functions, abstracting away the specific runtime adapter implementation.

use crate::time::{SleepCapable, TimestampProvider};
use core::future::Future;

/// Unified runtime context for AimDB services
///
/// This context provides access to essential runtime capabilities through
/// a clean, unified API. Services receive this context and can use it for
/// timing operations without needing to know about the underlying runtime.
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
    R: SleepCapable + TimestampProvider,
{
    runtime: R,
}

impl<R> RuntimeContext<R>
where
    R: SleepCapable + TimestampProvider,
{
    /// Create a new RuntimeContext with the given runtime adapter
    ///
    /// # Arguments
    ///
    /// * `runtime` - Runtime adapter implementing both SleepCapable and TimestampProvider traits
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

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
    pub fn sleep(&self, duration: R::Duration) -> impl Future<Output = ()> + '_ {
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
}

impl<R> RuntimeContext<R>
where
    R: SleepCapable + TimestampProvider + Clone,
{
    /// Create a RuntimeContext from a runtime adapter
    ///
    /// This is a convenience method for runtime adapters that implement both
    /// SleepCapable and TimestampProvider (like TokioAdapter and EmbassyAdapter).
    ///
    /// # Arguments
    ///
    /// * `runtime` - Runtime adapter implementing both required traits
    pub fn from_runtime(runtime: R) -> Self {
        Self::new(runtime)
    }
}

/// Create a RuntimeContext from any type that implements the required traits
///
/// This function provides a generic way to create a RuntimeContext from any
/// runtime adapter, as long as it implements the necessary time traits.
/// This is particularly useful in macros where we don't know the concrete type.
///
/// # Arguments
/// * `runtime` - Any type implementing SleepCapable + TimestampProvider + Clone
///
/// # Returns
/// A RuntimeContext wrapping the provided runtime
pub fn create_runtime_context<R>(runtime: R) -> RuntimeContext<R>
where
    R: SleepCapable + TimestampProvider + Clone,
{
    RuntimeContext::from_runtime(runtime)
}
