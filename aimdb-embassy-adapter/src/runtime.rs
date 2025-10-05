//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_core::{DbResult, DelayCapableAdapter, RuntimeAdapter, SpawnStatically};
use core::future::Future;

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

#[cfg(feature = "embassy-time")]
use embassy_time::{Duration, Timer};

#[cfg(feature = "embassy-runtime")]
use embassy_executor::Spawner;

/// Embassy runtime adapter for async task execution in embedded systems
///
/// This adapter provides AimDB's runtime interface for Embassy-based embedded
/// applications. It can either work standalone or store an Embassy spawner
/// for integrated task management.
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
/// # Ok(())
/// # }
/// # }
/// ```
pub struct EmbassyAdapter {
    #[cfg(feature = "embassy-runtime")]
    spawner: Option<Spawner>,
    #[cfg(not(feature = "embassy-runtime"))]
    _phantom: core::marker::PhantomData<()>,
}

// SAFETY: EmbassyAdapter only contains an Option<Spawner> and Spawner is thread-safe.
// Embassy executor handles spawner synchronization internally.
unsafe impl Send for EmbassyAdapter {}
unsafe impl Sync for EmbassyAdapter {}

impl core::fmt::Debug for EmbassyAdapter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut debug_struct = f.debug_struct("EmbassyAdapter");

        #[cfg(feature = "embassy-runtime")]
        debug_struct.field("spawner", &self.spawner.is_some());

        #[cfg(not(feature = "embassy-runtime"))]
        debug_struct.field("_phantom", &"no spawner support");

        debug_struct.finish()
    }
}

impl EmbassyAdapter {
    /// Creates a new EmbassyAdapter without a spawner
    ///
    /// This creates a stateless adapter suitable for basic task execution.
    ///
    /// # Returns
    /// `Ok(EmbassyAdapter)` - Always succeeds
    pub fn new() -> DbResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter (no spawner)");

        Ok(Self {
            #[cfg(feature = "embassy-runtime")]
            spawner: None,
            #[cfg(not(feature = "embassy-runtime"))]
            _phantom: core::marker::PhantomData,
        })
    }

    /// Creates a new EmbassyAdapter with an Embassy spawner
    ///
    /// This creates an adapter that can use the Embassy spawner for
    /// advanced task management operations.
    ///
    /// # Arguments
    /// * `spawner` - The Embassy spawner to use for task management
    ///
    /// # Returns
    /// An EmbassyAdapter configured with the provided spawner
    #[cfg(feature = "embassy-runtime")]
    pub fn new_with_spawner(spawner: Spawner) -> Self {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter with spawner");

        Self {
            spawner: Some(spawner),
        }
    }

    /// Gets a reference to the stored spawner, if any
    #[cfg(feature = "embassy-runtime")]
    pub fn spawner(&self) -> Option<&Spawner> {
        self.spawner.as_ref()
    }
}

impl Default for EmbassyAdapter {
    fn default() -> Self {
        Self::new().expect("EmbassyAdapter::default() should not fail")
    }
}

// Trait implementations for the core adapter interfaces

impl RuntimeAdapter for EmbassyAdapter {
    fn new() -> DbResult<Self> {
        Self::new()
    }

    fn runtime_name() -> &'static str {
        "embassy"
    }
}

#[cfg(feature = "embassy-runtime")]
impl SpawnStatically for EmbassyAdapter {
    fn spawner(&self) -> Option<&embassy_executor::Spawner> {
        self.spawner.as_ref()
    }
}

#[cfg(feature = "embassy-time")]
impl DelayCapableAdapter for EmbassyAdapter {
    type Duration = Duration;

    #[allow(clippy::manual_async_fn)]
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
