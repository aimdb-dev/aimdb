//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_core::{DbError, DbResult};
use aimdb_executor::{ExecutorResult, RuntimeAdapter};

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

#[cfg(feature = "embassy-runtime")]
use embassy_executor::Spawner;

#[cfg(feature = "embassy-net-support")]
use embassy_net::Stack;

/// Trait for accessing Embassy network stack
///
/// This trait provides connectors with access to the network stack for
/// creating TCP/UDP connections. It's implemented by EmbassyAdapter when
/// the `embassy-net-support` feature is enabled.
///
/// # Example
/// ```rust,ignore
/// use aimdb_embassy_adapter::EmbassyNetwork;
///
/// fn create_mqtt_connector<R: EmbassyNetwork>(runtime: &R) {
///     let network = runtime.network_stack();
///     // Use network to create TCP connection
/// }
/// ```
#[cfg(feature = "embassy-net-support")]
pub trait EmbassyNetwork {
    /// Get a reference to the Embassy network stack
    ///
    /// Returns a reference to the static network stack that can be used
    /// for creating network connections (TCP, UDP, etc.).
    fn network_stack(&self) -> &'static Stack<'static>;
}

/// Embassy runtime adapter for async task execution in embedded systems
///
/// This adapter provides AimDB's runtime interface for Embassy-based embedded
/// applications. It can either work standalone or store an Embassy spawner
/// for integrated task management.
///
/// When the `embassy-net-support` feature is enabled, it can also store a
/// reference to the network stack for connector use.
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
    #[cfg(feature = "embassy-net-support")]
    network: Option<&'static Stack<'static>>,
    #[cfg(not(any(feature = "embassy-runtime", feature = "embassy-net-support")))]
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

        #[cfg(feature = "embassy-net-support")]
        debug_struct.field("network", &self.network.is_some());

        #[cfg(not(any(feature = "embassy-runtime", feature = "embassy-net-support")))]
        debug_struct.field("_phantom", &"no features enabled");

        debug_struct.finish()
    }
}

impl EmbassyAdapter {
    /// Creates a new EmbassyAdapter without a spawner or network
    ///
    /// This creates a stateless adapter suitable for basic task execution.
    ///
    /// # Returns
    /// `Ok(EmbassyAdapter)` - Always succeeds
    pub fn new() -> ExecutorResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter (no spawner, no network)");

        Ok(Self {
            #[cfg(feature = "embassy-runtime")]
            spawner: None,
            #[cfg(feature = "embassy-net-support")]
            network: None,
            #[cfg(not(any(feature = "embassy-runtime", feature = "embassy-net-support")))]
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
            #[cfg(feature = "embassy-net-support")]
            network: None,
        }
    }

    /// Creates a new EmbassyAdapter with spawner and network stack
    ///
    /// This creates an adapter with full capabilities for task execution
    /// and network operations.
    ///
    /// # Arguments
    /// * `spawner` - The Embassy spawner to use for task management
    /// * `network` - Static reference to the network stack
    ///
    /// # Returns
    /// An EmbassyAdapter configured with spawner and network
    #[cfg(all(feature = "embassy-runtime", feature = "embassy-net-support"))]
    pub fn new_with_network(spawner: Spawner, network: &'static Stack<'static>) -> Self {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter with spawner and network");

        Self {
            spawner: Some(spawner),
            network: Some(network),
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

// Trait implementations for the simplified core adapter interfaces

impl RuntimeAdapter for EmbassyAdapter {
    fn runtime_name() -> &'static str {
        "embassy"
    }
}

// Type-erased future wrapper for dynamic spawning
#[cfg(feature = "embassy-runtime")]
type BoxedFuture = alloc::boxed::Box<dyn core::future::Future<Output = ()> + Send + 'static>;

// Configurable task pool size for dynamic spawning
// Users can select the pool size via feature flags:
// - embassy-task-pool-8 (default): 8 concurrent tasks
// - embassy-task-pool-16: 16 concurrent tasks
// - embassy-task-pool-32: 32 concurrent tasks
#[cfg(all(feature = "embassy-runtime", feature = "embassy-task-pool-32"))]
const TASK_POOL_SIZE: usize = 32;

#[cfg(all(
    feature = "embassy-runtime",
    feature = "embassy-task-pool-16",
    not(feature = "embassy-task-pool-32")
))]
const TASK_POOL_SIZE: usize = 16;

#[cfg(all(
    feature = "embassy-runtime",
    not(feature = "embassy-task-pool-16"),
    not(feature = "embassy-task-pool-32")
))]
const TASK_POOL_SIZE: usize = 8;

// Generic task runner for dynamic spawning in Embassy
// Pool size is configured at compile time via feature flags
#[cfg(feature = "embassy-runtime")]
#[embassy_executor::task(pool_size = TASK_POOL_SIZE)]
async fn generic_task_runner(mut future: BoxedFuture) {
    use core::pin::Pin;
    // Pin the boxed future so it can be awaited
    let pinned = unsafe { Pin::new_unchecked(&mut *future) };
    pinned.await;
}

// Implement Spawn trait for Embassy with dynamic spawning support
#[cfg(feature = "embassy-runtime")]
impl aimdb_executor::Spawn for EmbassyAdapter {
    type SpawnToken = (); // Embassy doesn't return a handle from static spawn

    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        // Box the future for type erasure
        let boxed_future: BoxedFuture = alloc::boxed::Box::new(future);

        // Get the spawner
        let spawner =
            self.spawner
                .as_ref()
                .ok_or(aimdb_executor::ExecutorError::RuntimeUnavailable {
                    message: "No spawner available - use EmbassyAdapter::new_with_spawner()",
                })?;

        // Spawn using our generic task pool
        match generic_task_runner(boxed_future) {
            Ok(spawn_token) => {
                spawner.spawn(spawn_token);
                Ok(())
            }
            Err(_) => Err(aimdb_executor::ExecutorError::SpawnFailed {
                message: "Task pool exhausted - enable a larger task pool feature (embassy-task-pool-16 or embassy-task-pool-32)",
            }),
        }
    }
}

// Implement TimeOps trait for time operations
#[cfg(feature = "embassy-time")]
impl aimdb_executor::TimeOps for EmbassyAdapter {
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

    fn sleep(&self, duration: Self::Duration) -> impl core::future::Future<Output = ()> + Send {
        embassy_time::Timer::after(duration)
    }
}

// Implement Logger trait
impl aimdb_executor::Logger for EmbassyAdapter {
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

// Implement EmbassyNetwork trait for accessing network stack
#[cfg(feature = "embassy-net-support")]
impl EmbassyNetwork for EmbassyAdapter {
    fn network_stack(&self) -> &'static Stack<'static> {
        self.network.expect(
            "Network stack not available - connectors requiring network access need the 'embassy-net-support' feature enabled and must use EmbassyAdapter::new_with_network() to provide a network stack",
        )
    }
}
