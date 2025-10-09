//! Tokio Runtime Adapter for AimDB
//!
//! This module provides the Tokio-specific implementation of AimDB's runtime traits,
//! enabling async task spawning and execution in std environments using Tokio.

use aimdb_core::{DbError, DbResult};
use aimdb_executor::{
    ExecutorResult, Logger, RuntimeAdapter, Spawn, TimeOps,
};
use core::future::Future;
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

/// Tokio runtime adapter for async task spawning in std environments
///
/// The TokioAdapter provides AimDB's runtime interface for Tokio-based
/// applications, focusing on task spawning capabilities that leverage Tokio's
/// async executor and scheduling infrastructure.
///
/// This module provides the core RuntimeAdapter and DelayCapableAdapter implementations.
/// For time-related capabilities (timestamps, sleep), see the `time` module.
///
/// # Features
/// - Task spawning with Tokio executor integration
/// - Delayed task spawning with `tokio::time::sleep`
/// - Tracing integration for observability (when `tracing` feature enabled)
/// - Zero-cost abstraction over Tokio's async runtime
///
/// # Example
/// ```rust,no_run
/// use aimdb_tokio_adapter::TokioAdapter;
/// use aimdb_core::RuntimeAdapter;
///
/// #[tokio::main]
/// async fn main() -> aimdb_core::DbResult<()> {
///     let adapter = TokioAdapter::new()?;
///     
///     let result = adapter.spawn_task(async {
///         Ok::<_, aimdb_core::DbError>(42)
///     }).await?;
///
///     // Result: 42
///     Ok(())
/// }
/// ```
#[cfg(feature = "tokio-runtime")]
#[derive(Debug, Clone, Copy)]
pub struct TokioAdapter;

#[cfg(feature = "tokio-runtime")]
impl TokioAdapter {
    /// Creates a new TokioAdapter
    ///
    /// # Returns
    /// `Ok(TokioAdapter)` - Tokio adapters are lightweight and cannot fail
    pub fn new() -> ExecutorResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating TokioAdapter");

        Ok(Self)
    }

    /// Creates a new TokioAdapter returning DbResult for backward compatibility
    ///
    /// This method provides compatibility with existing code that expects DbResult.
    pub fn new_db_result() -> DbResult<Self> {
        Self::new().map_err(DbError::from)
    }

    /// Spawns an async task on the Tokio executor
    ///
    /// This method provides the core task spawning functionality for Tokio-based
    /// applications. Tasks are spawned on the Tokio executor, which handles
    /// scheduling and execution across available threads.
    ///
    /// # Arguments
    /// * `task` - The async task to spawn
    ///
    /// # Returns
    /// `DbResult<T>` where T is the task's success type
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_core::RuntimeAdapter;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> aimdb_core::DbResult<()> {
    /// let adapter = TokioAdapter::new()?;
    ///
    /// let result = adapter.spawn_task(async {
    ///     // Some async work
    ///     Ok::<i32, aimdb_core::DbError>(42)
    /// }).await?;
    ///
    /// // Result: 42
    /// # Ok(())
    /// # }
    /// ```
    pub async fn spawn_task<F, T>(&self, task: F) -> DbResult<T>
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        #[cfg(feature = "tracing")]
        debug!("Spawning async task");

        let handle = tokio::task::spawn(task);

        match handle.await {
            Ok(result) => {
                #[cfg(feature = "tracing")]
                match &result {
                    Ok(_) => debug!("Async task completed successfully"),
                    Err(e) => warn!(?e, "Async task failed"),
                }
                result
            }
            Err(join_error) => {
                #[cfg(feature = "tracing")]
                warn!(?join_error, "Task join failed");

                use crate::TokioErrorSupport;
                Err(DbError::from_join_error(join_error))
            }
        }
    }
}

#[cfg(feature = "tokio-runtime")]
impl Default for TokioAdapter {
    fn default() -> Self {
        Self::new().expect("TokioAdapter::default() should not fail")
    }
}

// Trait implementations for the core adapter interfaces

#[cfg(feature = "tokio-runtime")]
impl RuntimeAdapter for TokioAdapter {
    fn runtime_name() -> &'static str {
        "tokio"
    }
}

// Implement Spawn trait for dynamic task spawning
#[cfg(feature = "tokio-runtime")]
impl Spawn for TokioAdapter {
    type SpawnToken = tokio::task::JoinHandle<()>;

    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        #[cfg(feature = "tracing")]
        tracing::debug!("Spawning future on Tokio runtime");

        Ok(tokio::spawn(future))
    }
}

#[cfg(feature = "tokio-runtime")]
// impl DelayCapableAdapter for TokioAdapter {
//     type Duration = Duration;
// 
//     /// Spawns a task that begins execution after the specified delay
//     ///
//     /// This implementation uses `tokio::time::sleep` to create the delay
//     /// before executing the provided task.
//     ///
//     /// # Arguments
//     /// * `task` - The async task to spawn after the delay
//     /// * `delay` - How long to wait before starting task execution
//     ///
//     /// # Returns
//     /// `DbResult<T>` where T is the task's success type
//     ///
//     /// # Example
//     /// ```rust,no_run
//     /// use aimdb_tokio_adapter::TokioAdapter;
//     /// use aimdb_executor::{DelayCapableAdapter, ExecutorResult};
//     /// use std::time::Duration;
//     ///
//     /// # #[tokio::main]
//     /// # async fn main() -> ExecutorResult<()> {
//     /// let adapter = TokioAdapter::new()?;
//     ///
//     /// let result = adapter.spawn_delayed_task(
//     ///     async { Ok::<i32, aimdb_executor::ExecutorError>(42) },
//     ///     Duration::from_millis(100)
//     /// ).await?;
//     ///
//     /// assert_eq!(result, 42);
//     /// # Ok(())
//     /// # }
//     /// ```
//     #[allow(clippy::manual_async_fn)]
//     fn spawn_delayed_task<F, T>(
//         &self,
//         task: F,
//         delay: Self::Duration,
//     ) -> impl Future<Output = ExecutorResult<T>> + Send
//     where
//         F: Future<Output = ExecutorResult<T>> + Send + 'static,
//         T: Send + 'static,
//     {
//         async move {
//             tokio::time::sleep(delay).await;
//             task.await
//         }
//     }
// }

// New unified Runtime trait implementations

#[cfg(feature = "tokio-runtime")]
impl TimeOps for TokioAdapter {
    type Instant = Instant;
    type Duration = Duration;

    fn now(&self) -> Self::Instant {
        Instant::now()
    }

    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<Self::Duration> {
        later.checked_duration_since(earlier)
    }

    fn millis(&self, millis: u64) -> Self::Duration {
        Duration::from_millis(millis)
    }

    fn secs(&self, secs: u64) -> Self::Duration {
        Duration::from_secs(secs)
    }

    fn micros(&self, micros: u64) -> Self::Duration {
        Duration::from_micros(micros)
    }

    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }
}

#[cfg(feature = "tokio-runtime")]
impl Logger for TokioAdapter {
    fn info(&self, message: &str) {
        println!("ℹ️  {}", message);
    }

    fn debug(&self, message: &str) {
        #[cfg(debug_assertions)]
        println!("🔍 {}", message);
        #[cfg(not(debug_assertions))]
        let _ = message; // Avoid unused variable warning in release
    }

    fn warn(&self, message: &str) {
        println!("⚠️  {}", message);
    }

    fn error(&self, message: &str) {
        eprintln!("❌ {}", message);
    }
}

// Runtime trait is auto-implemented when RuntimeAdapter + TimeOps + Logger + Spawn are implemented
