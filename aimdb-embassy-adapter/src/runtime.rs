//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task spawning and execution in embedded environments using Embassy.

use aimdb_core::{DbResult, DelayCapableAdapter, RuntimeAdapter};
use core::future::Future;

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

#[cfg(feature = "embassy-time")]
use embassy_time::{Duration, Timer};

/// Embassy runtime adapter for async task spawning in embedded systems
///
/// The EmbassyAdapter provides AimDB's runtime interface for Embassy-based embedded
/// applications, focusing on task spawning capabilities that leverage Embassy's
/// async executor and scheduling infrastructure.
///
/// # Features
/// - Task spawning with Embassy executor integration
/// - Delayed task spawning (when `embassy-time` feature enabled)
/// - Tracing integration for observability (when `tracing` feature enabled)
/// - Zero-allocation error handling for resource-constrained environments
///
/// # Example
/// ```rust,no_run
/// # #[cfg(not(feature = "std"))]
/// # {
/// use aimdb_embassy_adapter::EmbassyAdapter;
/// use aimdb_core::RuntimeAdapter;
///
/// # async fn example() -> aimdb_core::DbResult<()> {
/// let adapter = EmbassyAdapter::new()?;
///     
/// let result = adapter.spawn_task(async {
///     Ok::<_, aimdb_core::DbError>(42)
/// }).await?;
///
/// // Result: 42
/// # Ok(())
/// # }
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct EmbassyAdapter;

impl EmbassyAdapter {
    /// Creates a new EmbassyAdapter
    ///
    /// # Returns
    /// `Ok(EmbassyAdapter)` - Embassy adapters are lightweight and cannot fail
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(not(feature = "std"))]
    /// # {
    /// use aimdb_embassy_adapter::EmbassyAdapter;
    /// use aimdb_core::RuntimeAdapter;
    ///
    /// # fn example() -> aimdb_core::DbResult<()> {
    /// let adapter = EmbassyAdapter::new()?;
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn new() -> DbResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter");

        Ok(Self)
    }

    /// Spawns an async task on the Embassy executor
    ///
    /// This method provides the core task spawning functionality for Embassy-based
    /// applications. In Embassy environments, tasks are typically spawned using
    /// the executor's spawn functionality, but since we're in a library context,
    /// we simply execute the task directly as Embassy handles the scheduling.
    ///
    /// # Arguments
    /// * `task` - The async task to spawn
    ///
    /// # Returns
    /// `DbResult<T>` where T is the task's success type
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(not(feature = "std"))]
    /// # {
    /// use aimdb_embassy_adapter::EmbassyAdapter;
    /// use aimdb_core::RuntimeAdapter;
    ///
    /// # async fn example() -> aimdb_core::DbResult<()> {
    /// let adapter = EmbassyAdapter::new()?;
    ///
    /// let result = adapter.spawn_task(async {
    ///     // Some async work
    ///     Ok::<i32, aimdb_core::DbError>(42)
    /// }).await?;
    ///
    /// // Result: 42
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub async fn spawn_task<F, T>(&self, task: F) -> DbResult<T>
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        #[cfg(feature = "tracing")]
        debug!("Spawning async task");

        let result = task.await;

        #[cfg(feature = "tracing")]
        match &result {
            Ok(_) => debug!("Async task completed successfully"),
            Err(e) => warn!(?e, "Async task failed"),
        }

        result
    }
}

impl Default for EmbassyAdapter {
    fn default() -> Self {
        Self::new().expect("EmbassyAdapter::default() should not fail")
    }
}

// Trait implementations for the core adapter interfaces

impl RuntimeAdapter for EmbassyAdapter {
    fn spawn_task<F, T>(&self, task: F) -> impl Future<Output = DbResult<T>> + Send
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        self.spawn_task(task)
    }

    fn new() -> DbResult<Self> {
        Self::new()
    }
}

#[cfg(feature = "embassy-time")]
impl DelayCapableAdapter for EmbassyAdapter {
    type Duration = Duration;

    fn spawn_delayed_task<F, T>(
        &self,
        task: F,
        delay: Self::Duration,
    ) -> impl Future<Output = DbResult<T>> + Send
    where
        F: Future<Output = DbResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        async move {
            Timer::after(delay).await;
            task.await
        }
    }
}
