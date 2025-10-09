//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_core::{DbError, DbResult};
use aimdb_executor::{ExecutorResult, Logger, RuntimeAdapter, Spawn, TimeOps};
use core::future::Future;

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

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
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter (no spawner)");

        Ok(Self {
            #[cfg(feature = "embassy-runtime")]
            spawner: None,
            #[cfg(not(feature = "embassy-runtime"))]
            _phantom: core::marker::PhantomData,
        })
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

    /// Initialize a global static adapter instance
    ///
    /// This provides a cleaner API by managing the static storage internally.
    /// Only call this once at the start of your application.
    ///
    /// # Safety
    /// Must be called only once during application initialization, before any services are spawned.
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(not(feature = "std"))]
    /// # {
    /// use aimdb_embassy_adapter::EmbassyAdapter;
    /// use embassy_executor::Spawner;
    ///
    /// #[embassy_executor::main]
    /// async fn main(spawner: Spawner) -> ! {
    ///     // Initialize the global adapter
    ///     EmbassyAdapter::init_global(spawner);
    ///     
    ///     // Get a reference to use with services
    ///     let adapter = EmbassyAdapter::global();
    ///     
    ///     // Use adapter to spawn services...
    ///     # loop {}
    /// }
    /// # }
    /// ```
    #[cfg(feature = "embassy-runtime")]
    pub fn init_global(spawner: Spawner) {
        static mut GLOBAL_ADAPTER: Option<EmbassyAdapter> = None;

        unsafe {
            use core::ptr;
            ptr::write(
                ptr::addr_of_mut!(GLOBAL_ADAPTER),
                Some(Self::new_with_spawner(spawner)),
            );
        }
    }

    /// Get a reference to the global adapter instance
    ///
    /// # Panics
    /// Panics if `init_global()` has not been called first.
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(not(feature = "std"))]
    /// # {
    /// use aimdb_embassy_adapter::EmbassyAdapter;
    ///
    /// # async fn example() {
    /// let adapter = EmbassyAdapter::global();
    /// // Use adapter with services...
    /// # }
    /// # }
    /// ```
    #[cfg(feature = "embassy-runtime")]
    pub fn global() -> &'static Self {
        static mut GLOBAL_ADAPTER: Option<EmbassyAdapter> = None;

        unsafe {
            use core::ptr;
            ptr::addr_of!(GLOBAL_ADAPTER)
                .as_ref()
                .and_then(|opt| opt.as_ref())
                .expect("EmbassyAdapter::init_global() must be called first")
        }
    }
}

impl Default for EmbassyAdapter {
    fn default() -> Self {
        Self::new().expect("EmbassyAdapter::default() should not fail")
    }
}

// Trait implementations for the simplified core adapter interfaces

impl RuntimeAdapter for EmbassyAdapter {
    fn runtime_name() -> &'static str {
        "embassy"
    }
}

// Implement Spawn trait for Embassy (static spawning)
#[cfg(feature = "embassy-runtime")]
impl Spawn for EmbassyAdapter {
    type SpawnToken = (); // Embassy doesn't return a handle from static spawn

    fn spawn<F>(&self, _future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Embassy spawning is handled via the #[embassy_executor::task] attribute
        // and spawner.spawn() at the application level. This method exists for
        // trait compatibility but spawning must be done through Embassy tasks.
        Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
            message: "Embassy requires static task spawning via #[embassy_executor::task]",
        })
    }
}

// Implement TimeOps trait (combines TimeSource + Sleeper)
#[cfg(feature = "embassy-time")]
impl TimeOps for EmbassyAdapter {
    type Instant = embassy_time::Instant;
    type Duration = embassy_time::Duration;

    fn now(&self) -> Self::Instant {
        embassy_time::Instant::now()
    }

    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<Self::Duration> {
        if later >= earlier {
            Some(later - earlier)
        } else {
            None
        }
    }

    fn millis(&self, millis: u64) -> Self::Duration {
        embassy_time::Duration::from_millis(millis)
    }

    fn secs(&self, secs: u64) -> Self::Duration {
        embassy_time::Duration::from_secs(secs)
    }

    fn micros(&self, micros: u64) -> Self::Duration {
        embassy_time::Duration::from_micros(micros)
    }

    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        embassy_time::Timer::after(duration)
    }
}

// Implement Logger trait
impl Logger for EmbassyAdapter {
    fn info(&self, message: &str) {
        defmt::info!("{}", message);
    }

    fn debug(&self, message: &str) {
        defmt::debug!("{}", message);
    }

    fn warn(&self, message: &str) {
        defmt::warn!("{}", message);
    }

    fn error(&self, message: &str) {
        defmt::error!("{}", message);
    }
}

// Runtime trait is auto-implemented when RuntimeAdapter + TimeOps + Logger + Spawn are all implemented
