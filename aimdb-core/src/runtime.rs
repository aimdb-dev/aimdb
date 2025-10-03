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
/// - **Platform Flexible**: Works across std and no_std environments  
/// - **Performance Focused**: Zero-cost abstractions where possible
/// - **Error Preserving**: Maintains full error context through async chains
/// - **Task Spawning**: Uses spawn semantics to leverage runtime schedulers
///
/// # Implementations
///
/// - `TokioAdapter`: For std environments using Tokio runtime
/// - `EmbassyAdapter`: For no_std embedded environments using Embassy
///
/// # Example
///
/// ```rust,no_run
/// use aimdb_core::{RuntimeAdapter, DbResult};
///
/// async fn execute_database_operation<A: RuntimeAdapter>(
///     adapter: &A,
/// ) -> DbResult<String> {
///     adapter.spawn_task(async {
///         // Database operation here
///         Ok("Operation completed".to_string())
///     }).await
/// }
/// ```
pub trait RuntimeAdapter {
    /// Spawns an async task on the runtime and waits for its completion
    ///
    /// This is the core method that all adapters must implement to provide
    /// task spawning capabilities with proper error handling and lifecycle management.
    /// The `'static` lifetime bounds ensure tasks can be safely spawned without
    /// lifetime dependencies.
    ///
    /// # Arguments
    /// * `task` - The async task to spawn and execute
    ///
    /// # Returns
    /// `DbResult<T>` where T is the task's success type
    ///
    /// # Errors
    /// - Task spawn failures converted to `DbError`
    /// - Task cancellation or panic errors
    /// - Any error propagated from the spawned task
    fn spawn_task<F, T>(&self, task: F) -> impl Future<Output = DbResult<T>> + Send
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static;

    /// Creates a new adapter instance with default configuration
    ///
    /// # Returns
    /// `Ok(Self)` on success, or `DbError` if adapter cannot be initialized
    fn new() -> DbResult<Self>
    where
        Self: Sized;
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "std")]
    mod std_tests {
        use crate::{DbResult, RuntimeAdapter};
        use std::future::Future;

        // Mock adapter for testing trait definitions
        struct MockAdapter;

        impl RuntimeAdapter for MockAdapter {
            fn spawn_task<F, T>(&self, task: F) -> impl Future<Output = DbResult<T>> + Send
            where
                F: Future<Output = DbResult<T>> + Send + 'static,
                T: Send + 'static,
            {
                task
            }

            fn new() -> DbResult<Self> {
                Ok(MockAdapter)
            }
        }

        #[tokio::test]
        async fn test_runtime_adapter_trait() {
            let adapter = MockAdapter::new().unwrap();

            let result = adapter
                .spawn_task(async { Ok::<i32, crate::DbError>(42) })
                .await;

            assert_eq!(result.unwrap(), 42);
        }
    }
}
