//! Runtime adapter trait definitions for AimDB
//!
//! This module defines the core traits that runtime-specific adapters must implement
//! to provide async execution capabilities across different environments.

use crate::DbResult;
use core::future::Future;

/// Core trait for runtime adapters providing async execution capabilities
///
/// This trait defines the essential interface that all AimDB runtime adapters
/// must implement, enabling the database to run on different async runtimes
/// while maintaining consistent behavior.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: The core database doesn't depend on specific runtimes
/// - **Service Focused**: Adapts to different service spawning models
/// - **Platform Flexible**: Works across std and no_std environments  
/// - **Performance Focused**: Zero-cost abstractions where possible
/// - **Error Preserving**: Maintains full error context through async chains
///
/// # Implementations
///
/// - `TokioAdapter`: For std environments using Tokio runtime
/// - `EmbassyAdapter`: For no_std embedded environments using Embassy
///
pub trait RuntimeAdapter {
    /// Spawns a service using the runtime's service management system
    ///
    /// This method handles service spawning in a runtime-appropriate way:
    /// - **Tokio**: Uses tokio::spawn with the service future
    /// - **Embassy**: Uses spawner.spawn with pre-defined service tasks
    ///
    /// # Type Parameters
    /// * `S` - The service type that implements ServiceSpawnable for this adapter
    ///
    /// # Arguments
    /// * `service_params` - Parameters needed for service initialization
    ///
    /// # Returns
    /// `DbResult<()>` indicating whether the service was successfully started
    fn spawn_service<S>(&self, service_params: ServiceParams) -> DbResult<()>
    where
        S: ServiceSpawnable<Self>,
        Self: Sized;

    /// Creates a new adapter instance with default configuration
    ///
    /// # Returns
    /// `Ok(Self)` on success, or `DbError` if adapter cannot be initialized
    fn new() -> DbResult<Self>
    where
        Self: Sized;
}

/// Parameters for service spawning
///
/// This struct contains the information needed to spawn a service,
/// allowing different runtimes to handle service initialization appropriately.
#[derive(Debug, Clone)]
pub struct ServiceParams {
    /// Service identifier/name for logging and debugging
    pub service_name: &'static str,
    /// Record that the service should operate on
    pub record: crate::Record,
    /// Any additional runtime-specific configuration
    pub config: ServiceConfig,
}

/// Configuration for service spawning
///
/// This allows runtime-specific configuration to be passed through
/// the generic service spawning interface.
#[derive(Debug, Clone)]
pub enum ServiceConfig {
    /// No additional configuration needed
    Default,
    /// Tokio-specific configuration (if any)
    #[cfg(feature = "std")]
    Tokio(TokioServiceConfig),
    /// Embassy-specific configuration
    #[cfg(not(feature = "std"))]
    Embassy(EmbassyServiceConfig),
}

/// Trait for services that can be spawned on runtime adapters
///
/// This trait is implemented by the service macro for each service,
/// providing a clean interface between the generated service code
/// and the runtime adapter.
pub trait ServiceSpawnable<A: RuntimeAdapter> {
    /// Spawns this service using the provided runtime adapter
    ///
    /// # Arguments
    /// * `adapter` - The runtime adapter to use for spawning
    /// * `params` - Service parameters and configuration
    ///
    /// # Returns
    /// `DbResult<()>` indicating whether spawning was successful
    fn spawn_with_adapter(adapter: &A, params: ServiceParams) -> crate::DbResult<()>;
}

#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct TokioServiceConfig {
    /// Task name for debugging
    pub task_name: Option<&'static str>,
}

#[cfg(not(feature = "std"))]
#[derive(Debug, Clone)]
pub struct EmbassyServiceConfig {
    /// Embassy-specific configuration options
    pub priority: Option<u8>,
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
