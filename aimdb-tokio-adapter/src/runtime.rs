//! Tokio Runtime Adapter for AimDB
//!
//! This module provides the Tokio-specific implementation of AimDB's runtime traits,
//! enabling async task spawning and execution in std environments using Tokio.

use aimdb_core::{DbError, DbResult};
use aimdb_executor::{Logger, RuntimeAdapter, TimeOps};
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
    /// Creates a new TokioAdapter instance
    ///
    /// This is a simple constructor that returns a TokioAdapter.
    /// Always succeeds since TokioAdapter is a zero-sized type.
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_tokio_adapter::TokioAdapter;
    ///
    /// let adapter = TokioAdapter::new()?;
    /// # Ok::<(), aimdb_core::DbError>(())
    /// ```
    pub fn new() -> DbResult<Self> {
        Ok(Self)
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

    fn duration_as_nanos(&self, duration: Self::Duration) -> u64 {
        duration.as_nanos().min(u64::MAX as u128) as u64
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        // The OS wall clock. `now()` is monotonic (for durations); this is the
        // absolute time AimX metadata timestamps report.
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| (d.as_secs(), d.subsec_nanos()))
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

// Runtime trait is auto-implemented when RuntimeAdapter + TimeOps + Logger are implemented
