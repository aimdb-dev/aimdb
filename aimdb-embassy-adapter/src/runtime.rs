//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_core::{DbError, DbResult};
#[cfg(feature = "embassy-time")]
use aimdb_executor::DelayCapableAdapter;
#[cfg(feature = "embassy-runtime")]
use aimdb_executor::SpawnStatically;
use aimdb_executor::{ExecutorResult, Runtime, RuntimeAdapter, Sleeper, TimeSource};
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
#[derive(Clone)]
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
    pub fn new() -> ExecutorResult<Self> {
        <Self as RuntimeAdapter>::new()
    }

    /// Creates a new EmbassyAdapter returning DbResult for backward compatibility
    ///
    /// This method provides compatibility with existing code that expects DbResult.
    pub fn new_db_result() -> DbResult<Self> {
        Self::new().map_err(DbError::from)
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
    fn new() -> ExecutorResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter (no spawner)");

        Ok(Self {
            #[cfg(feature = "embassy-runtime")]
            spawner: None,
            #[cfg(not(feature = "embassy-runtime"))]
            _phantom: core::marker::PhantomData,
        })
    }

    fn runtime_name() -> &'static str {
        "embassy"
    }
}

#[cfg(feature = "embassy-runtime")]
impl SpawnStatically for EmbassyAdapter {
    type Spawner = embassy_executor::Spawner;

    fn spawner(&self) -> Option<&Self::Spawner> {
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
    ) -> impl Future<Output = ExecutorResult<T>> + Send
    where
        F: Future<Output = ExecutorResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        async move {
            Timer::after(delay).await;
            task.await
        }
    }
}

// New unified Runtime trait implementations

#[cfg(feature = "embassy-time")]
impl TimeSource for EmbassyAdapter {
    type Instant = embassy_time::Instant;

    fn now(&self) -> Self::Instant {
        embassy_time::Instant::now()
    }

    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<core::time::Duration> {
        if later >= earlier {
            Some((later - earlier).into())
        } else {
            None
        }
    }
}

#[cfg(feature = "embassy-time")]
impl Sleeper for EmbassyAdapter {
    fn sleep(&self, duration: core::time::Duration) -> impl Future<Output = ()> + Send {
        embassy_time::Timer::after(embassy_time::Duration::from_micros(
            duration.as_micros() as u64
        ))
    }
}

#[cfg(all(feature = "embassy-time", feature = "embassy-runtime"))]
impl Runtime for EmbassyAdapter {
    fn has_dynamic_spawn(&self) -> bool {
        false
    }

    fn has_static_spawn(&self) -> bool {
        true
    }
}
