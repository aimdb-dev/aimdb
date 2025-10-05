//! Runtime adapter trait definitions for AimDB
//!
//! This module defines the core traits that runtime-specific adapters must implement
//! to provide async execution capabilities across different environments.

use crate::DbResult;
use core::future::Future;

/// Core trait for runtime adapters providing basic metadata and initialization
///
/// This trait defines the minimal interface that all AimDB runtime adapters
/// must implement. Specific spawning capabilities are provided by either
/// `SpawnDynamically` or `SpawnStatically` traits.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: The core database doesn't depend on specific runtimes
/// - **Clear Separation**: Dynamic vs static spawning models are explicit
/// - **Platform Flexible**: Works across std and no_std environments  
/// - **Performance Focused**: Zero-cost abstractions where possible
/// - **Error Preserving**: Maintains full error context through async chains
///
/// # Implementations
///
/// - `TokioAdapter`: Implements `SpawnDynamically` for std environments
/// - `EmbassyAdapter`: Implements `SpawnStatically` for no_std embedded environments
///
pub trait RuntimeAdapter: Send + Sync + 'static {
    /// Creates a new adapter instance with default configuration
    ///
    /// # Returns
    /// `Ok(Self)` on success, or `DbError` if adapter cannot be initialized
    fn new() -> DbResult<Self>
    where
        Self: Sized;

    /// Returns the runtime name for debugging and logging
    fn runtime_name() -> &'static str
    where
        Self: Sized;
}

/// Trait for runtimes that support dynamic future spawning (like Tokio)
///
/// This trait is for runtimes that can spawn arbitrary futures at runtime,
/// typically using a dynamic task scheduler.
#[cfg(feature = "tokio-runtime")]
pub trait SpawnDynamically: RuntimeAdapter {
    /// Spawns a future dynamically on the runtime
    ///
    /// # Type Parameters
    /// * `F` - The future to spawn
    /// * `T` - The return type of the future
    ///
    /// # Arguments
    /// * `future` - The async task to spawn
    ///
    /// # Returns
    /// A handle to the spawned task or an error if spawning failed
    fn spawn<F, T>(&self, future: F) -> DbResult<tokio::task::JoinHandle<T>>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;
}

/// Trait for runtimes that require compile-time task definition (like Embassy)
///
/// This trait is for runtimes that require tasks to be defined at compile time
/// and spawned through a spawner interface.
#[cfg(feature = "embassy-runtime")]
pub trait SpawnStatically: RuntimeAdapter {
    /// Gets access to the Embassy spawner for task spawning
    ///
    /// # Returns
    /// Reference to the spawner if available, None if no spawner is configured
    fn spawner(&self) -> Option<&embassy_executor::Spawner>;
}

/// Information about a runtime adapter
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// Name of the runtime (e.g., "tokio", "embassy")
    pub name: &'static str,
    /// Whether this runtime supports dynamic spawning
    pub supports_dynamic_spawn: bool,
    /// Whether this runtime supports static spawning
    pub supports_static_spawn: bool,
}

/// Core trait that all AimDB services must implement
///
/// This trait defines the essential interface for long-running services.
/// It is automatically implemented by the `#[service]` macro for service functions.
pub trait AimDbService: Send + Sync + 'static {
    /// Runs the service - this is the main service function
    ///
    /// Services should typically contain an infinite loop and handle their own
    /// error recovery. The service should only return if it's meant to terminate.
    fn run() -> impl Future<Output = DbResult<()>> + Send + 'static;

    /// Get the service name for logging and debugging
    fn service_name() -> &'static str;

    /// Spawn this service on a dynamic runtime (like Tokio)
    ///
    /// This method is automatically implemented by the service macro when
    /// tokio-runtime feature is enabled.
    #[cfg(feature = "tokio-runtime")]
    fn spawn_on_tokio(adapter: &impl SpawnDynamically) -> DbResult<()> {
        adapter.spawn(Self::run()).map(|_| ())
    }

    /// Spawn this service on a static runtime (like Embassy)
    ///
    /// This method must be implemented by the service macro when
    /// embassy-runtime feature is enabled, as it requires access to
    /// compile-time generated task functions.
    #[cfg(feature = "embassy-runtime")]
    fn spawn_on_embassy(adapter: &impl SpawnStatically) -> DbResult<()>;
}

/// Trait for adapters that support delayed task spawning
///
/// This trait provides the capability to spawn tasks that begin execution
/// after a specified delay, useful for scheduling and timing-based operations.
///
/// # Availability
/// - **Tokio environments**: Full support with `tokio::time::sleep()`
/// - **Embassy environments**: Available when `embassy-time` feature is enabled
/// - **Basic environments**: Not available
pub trait DelayCapableAdapter: RuntimeAdapter {
    /// Type representing a duration for this runtime
    type Duration;

    /// Spawns a task that begins execution after the specified delay
    ///
    /// This method combines task spawning with delay scheduling, allowing
    /// tasks to be queued for future execution.
    ///
    /// # Arguments
    /// * `task` - The async task to spawn after the delay
    /// * `delay` - How long to wait before starting task execution
    ///
    /// # Returns
    /// `DbResult<T>` where T is the task's success type
    ///
    /// # Errors
    /// - Task spawn failures converted to `DbError`
    /// - Delay/timing errors from the runtime
    /// - Any error propagated from the delayed task
    fn spawn_delayed_task<F, T>(
        &self,
        task: F,
        delay: Self::Duration,
    ) -> impl Future<Output = DbResult<T>> + Send
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static;
}
