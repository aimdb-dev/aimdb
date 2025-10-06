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
pub trait AimDbService: Send + Sync + 'static {
    /// Runs the service - this is the main service function
    ///
    /// Services should typically contain an infinite loop and handle their own
    /// error recovery. The service should only return if it's meant to terminate.
    fn run() -> impl Future<Output = ExecutorResult<()>> + Send + 'static;

    /// Get the service name for logging and debugging
    fn service_name() -> &'static str;

    /// Spawn this service on a dynamic runtime (like Tokio)
    ///
    /// This method is automatically implemented by the service macro.
    /// The implementation may have additional trait bounds depending on the service requirements.
    fn spawn_on_dynamic<R: SpawnDynamically>(adapter: &R) -> ExecutorResult<()>;

    /// Spawn this service on a static runtime (like Embassy)  
    ///
    /// This method is automatically implemented by the service macro.
    /// The implementation may have additional trait bounds depending on the service requirements.
    fn spawn_on_static<R: SpawnStatically>(adapter: &R) -> ExecutorResult<()>;
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
