//! AimDB Executor Traits
//!
//! This crate provides pure trait definitions for async execution across different
//! runtime environments. It enables dependency inversion where the core database
//! depends on abstractions rather than concrete runtime implementations.
//!
//! # Design Philosophy
//!
//! - **Runtime Agnostic**: No concrete runtime dependencies in trait definitions
//! - **Clear Separation**: Dynamic vs static spawning models are explicit
//! - **Platform Flexible**: Works across std and no_std environments
//! - **Performance Focused**: Zero-cost abstractions where possible
//! - **Dependency Inversion**: Core depends on traits, adapters provide implementations
//!
//! # Architecture
//!
//! ```text
//! aimdb-executor (this crate)     ← Pure trait definitions
//!       ↑              ↑
//!       |              |
//! tokio-adapter   embassy-adapter  ← Concrete implementations  
//!       ↑              ↑
//!       |              |
//!       └─── aimdb-core ───┘      ← Uses any executor via traits
//! ```
//!
//! # Usage
//!
//! This crate is typically not used directly. Instead:
//! 1. Runtime adapters implement these traits
//! 2. Core database accepts any type implementing the traits
//! 3. Applications choose their runtime adapter
//!
//! # Features
//!
//! - `std`: Enables error types that require std library
//! - `tokio-types`: Includes Tokio-specific types in trait signatures
//! - `embassy-types`: Includes Embassy-specific types in trait signatures

#![cfg_attr(not(feature = "std"), no_std)]

use core::future::Future;
use core::time::Duration;

// Error handling - use Result<T, E> generically since we can't depend on aimdb-core
pub type ExecutorResult<T> = Result<T, ExecutorError>;

/// Generic executor error type
///
/// This is a simple error type that can be converted to/from specific
/// database errors by the runtime adapters.
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(thiserror::Error))]
pub enum ExecutorError {
    #[cfg_attr(feature = "std", error("Spawn failed: {message}"))]
    SpawnFailed {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        message: &'static str,
    },

    #[cfg_attr(feature = "std", error("Runtime unavailable: {message}"))]
    RuntimeUnavailable {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        message: &'static str,
    },

    #[cfg_attr(feature = "std", error("Task join failed: {message}"))]
    TaskJoinFailed {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        message: &'static str,
    },
}

/// Core trait for runtime adapters providing basic metadata and initialization
///
/// This trait defines the minimal interface that all AimDB runtime adapters
/// must implement. Specific spawning capabilities are provided by either
/// `SpawnDynamically` or `SpawnStatically` traits.
///
/// # Implementations
///
/// - `TokioAdapter`: Implements `SpawnDynamically` for std environments
/// - `EmbassyAdapter`: Implements `SpawnStatically` for no_std embedded environments
pub trait RuntimeAdapter: Send + Sync + 'static {
    /// Creates a new adapter instance with default configuration
    ///
    /// # Returns
    /// `Ok(Self)` on success, or `ExecutorError` if adapter cannot be initialized
    fn new() -> ExecutorResult<Self>
    where
        Self: Sized;

    /// Returns the runtime name for debugging and logging
    fn runtime_name() -> &'static str
    where
        Self: Sized;
}

/// Trait for providing time information
///
/// This trait abstracts over different time representations across runtimes.
/// Implementations should use the most appropriate time type for their platform.
pub trait TimeSource: RuntimeAdapter {
    /// The instant type used by this runtime
    type Instant: Clone + Send + Sync + 'static;

    /// Get the current time instant
    fn now(&self) -> Self::Instant;

    /// Calculate the duration between two instants
    ///
    /// Returns None if `later` is before `earlier`
    fn duration_since(&self, later: Self::Instant, earlier: Self::Instant) -> Option<Duration>;
}

/// Trait for async sleep capability
///
/// This trait abstracts over different sleep implementations across runtimes.
pub trait Sleeper: RuntimeAdapter {
    /// Sleep for the specified duration
    ///
    /// # Arguments
    /// * `duration` - How long to sleep
    ///
    /// # Returns
    /// A future that completes after the duration has elapsed
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// Trait for runtimes that support dynamic future spawning (like Tokio)
///
/// This trait is for runtimes that can spawn arbitrary futures at runtime,
/// typically using a dynamic task scheduler.
pub trait SpawnDynamically: RuntimeAdapter {
    /// Handle type returned when spawning tasks
    type JoinHandle<T>: Send + 'static
    where
        T: Send + 'static;

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
    fn spawn<F, T>(&self, future: F) -> ExecutorResult<Self::JoinHandle<T>>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;
}

/// Trait for runtimes that require compile-time task definition (like Embassy)
///
/// This trait is for runtimes that require tasks to be defined at compile time
/// and spawned through a spawner interface.
pub trait SpawnStatically: RuntimeAdapter {
    /// Spawner type for this runtime
    type Spawner;

    /// Gets access to the spawner for task spawning
    ///
    /// # Returns
    /// Reference to the spawner if available, None if no spawner is configured
    fn spawner(&self) -> Option<&Self::Spawner>;
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
///
/// Services receive a RuntimeContext that provides access to time, sleep, and
/// other runtime capabilities in a platform-agnostic way.
pub trait AimDbService {
    /// The runtime type this service is compatible with
    type Runtime: Runtime;

    /// The error type returned by this service
    type Error: Send + 'static;

    /// Runs the service - this is the main service function
    ///
    /// Services should typically contain an infinite loop and handle their own
    /// error recovery. The service should only return if it's meant to terminate.
    ///
    /// # Arguments
    /// * `ctx` - Runtime context providing time, sleep, and other capabilities
    fn run(
        ctx: impl AsRef<Self::Runtime> + Send + 'static,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static;

    /// Get the service name for logging and debugging
    fn service_name() -> &'static str;
}

/// Simplified trait bundle for common runtime operations
///
/// This trait bundles the most commonly needed runtime capabilities.
/// Unlike requiring both dynamic AND static spawning, this focuses on
/// the capabilities most services actually need.
///
/// # Design Philosophy
///
/// Services typically need either dynamic OR static spawning, not both.
/// This trait focuses on runtime adapter identification and basic capabilities.
pub trait CommonRuntimeTraits: RuntimeAdapter {
    /// Whether this runtime supports dynamic spawning
    fn supports_dynamic_spawning() -> bool;

    /// Whether this runtime supports static spawning  
    fn supports_static_spawning() -> bool;
}

/// Unified runtime trait combining all runtime capabilities
///
/// This trait is the primary interface for runtime-agnostic code.
/// It combines time, sleep, and spawning capabilities into one cohesive interface.
///
/// # Usage
///
/// Services and database components should depend on this trait rather than
/// concrete adapter types. This enables testing with mock runtimes and
/// platform flexibility.
///
/// # Example
///
/// ```ignore
/// async fn my_service<R: Runtime>(runtime: &R) {
///     let start = runtime.now();
///     runtime.sleep(Duration::from_secs(1)).await;
///     let elapsed = runtime.duration_since(runtime.now(), start);
/// }
/// ```
pub trait Runtime: TimeSource + Sleeper {
    /// Type-erased runtime information
    fn info(&self) -> RuntimeInfo
    where
        Self: Sized,
    {
        RuntimeInfo {
            name: Self::runtime_name(),
            supports_dynamic_spawn: self.has_dynamic_spawn(),
            supports_static_spawn: self.has_static_spawn(),
        }
    }

    /// Check if this runtime supports dynamic spawning
    fn has_dynamic_spawn(&self) -> bool {
        false
    }

    /// Check if this runtime supports static spawning
    fn has_static_spawn(&self) -> bool {
        false
    }
}

/// Helper function to spawn a service on a dynamic runtime
///
/// # Type Parameters
/// * `S` - The service type to spawn
/// * `R` - The runtime adapter type
///
/// # Arguments
/// * `adapter` - The runtime adapter instance
/// * `context` - Runtime context to pass to the service
///
/// # Returns
/// `Ok(())` if spawning succeeded, error otherwise
pub fn spawn_service_dynamic<S, R, Ctx>(
    adapter: &R,
    context: Ctx,
) -> ExecutorResult<R::JoinHandle<Result<(), S::Error>>>
where
    S: AimDbService<Runtime = R>,
    R: SpawnDynamically + Runtime,
    Ctx: AsRef<R> + Send + 'static,
{
    adapter.spawn(S::run(context))
}

/// Helper function to spawn a service on a static runtime
///
/// # Type Parameters  
/// * `S` - The service type to spawn
/// * `R` - The runtime adapter type
///
/// # Arguments
/// * `adapter` - The runtime adapter instance
///
/// # Returns
/// `Ok(())` if spawning succeeded, error otherwise
///
/// # Note
/// Static spawning requires compile-time task definitions (e.g., Embassy tasks).
/// The service macro handles this automatically.
pub fn spawn_service_static<S, R>(_adapter: &R) -> ExecutorResult<()>
where
    S: AimDbService<Runtime = R>,
    R: SpawnStatically + Runtime,
{
    // Static spawning is handled by the macro-generated Embassy task
    // This is just a placeholder that will be called by the macro
    Err(ExecutorError::RuntimeUnavailable {
        #[cfg(feature = "std")]
        message: "Static spawning must be done through macro-generated Embassy tasks".to_string(),
        #[cfg(not(feature = "std"))]
        message: "Static spawning must be done through macro-generated Embassy tasks",
    })
}

/// Dynamic runtime trait for runtimes that support dynamic task spawning
///
/// This is the trait bundle most std-environment runtimes will implement.
pub trait DynamicRuntimeTraits: RuntimeAdapter + SpawnDynamically + CommonRuntimeTraits {}

// Auto-implement for dynamic runtimes
impl<T> DynamicRuntimeTraits for T where T: RuntimeAdapter + SpawnDynamically + CommonRuntimeTraits {}

/// Static runtime trait for runtimes that require compile-time task definition
///
/// This is the trait bundle most no_std embedded runtimes will implement.
pub trait StaticRuntimeTraits: RuntimeAdapter + SpawnStatically + CommonRuntimeTraits {}

// Auto-implement for static runtimes
impl<T> StaticRuntimeTraits for T where T: RuntimeAdapter + SpawnStatically + CommonRuntimeTraits {}

/// Trait for adapters that support delayed task spawning
///
/// This trait provides the capability to spawn tasks that begin execution
/// after a specified delay, useful for scheduling and timing-based operations.
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
    /// `ExecutorResult<T>` where T is the task's success type
    ///
    /// # Errors
    /// - Task spawn failures converted to `ExecutorError`
    /// - Delay/timing errors from the runtime
    /// - Any error propagated from the delayed task
    fn spawn_delayed_task<F, T>(
        &self,
        task: F,
        delay: Self::Duration,
    ) -> impl Future<Output = ExecutorResult<T>> + Send
    where
        F: Future<Output = ExecutorResult<T>> + Send + 'static,
        T: Send + 'static;
}
