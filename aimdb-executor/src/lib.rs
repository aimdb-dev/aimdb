//! AimDB Executor Traits
//!
//! Pure trait definitions for async execution across different runtime environments.
//! Enables dependency inversion where the core database depends on abstractions
//! rather than concrete runtime implementations.
//!
//! # Design Philosophy
//!
//! - **Runtime Agnostic**: No concrete runtime dependencies
//! - **Simple Trait Structure**: 3 focused traits covering all runtime needs
//! - **Platform Flexible**: Works across std and no_std environments
//! - **Zero Dependencies**: Pure trait definitions with minimal coupling
//!
//! # Trait Structure
//!
//! 1. **`RuntimeAdapter`** - Platform identity and metadata
//! 2. **`TimeOps`** - Time operations (now, sleep, duration helpers)
//! 3. **`Logger`** - Structured logging (info, debug, warn, error)
//! 4. **`RuntimeOps`** - Object-safe bundle of the above (`Arc<dyn RuntimeOps>`)
//!    for code that holds the runtime as a value instead of a type parameter
//!
//! Task execution is driven by the `AimDbRunner` returned from
//! `AimDbBuilder::build()`, which collects every future the database needs
//! into a single `FuturesUnordered` — no per-adapter `Spawn` impl required.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use core::future::Future;

pub mod ops;
#[doc(hidden)]
pub mod test_support;

pub use ops::{BoxFuture, LogLevel, RuntimeOps};

// ============================================================================
// Error Types
// ============================================================================

pub type ExecutorResult<T> = Result<T, ExecutorError>;

#[derive(Debug)]
#[cfg_attr(feature = "std", derive(thiserror::Error))]
pub enum ExecutorError {
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

    #[cfg_attr(feature = "std", error("Join queue closed"))]
    QueueClosed,
}

// ============================================================================
// Core Traits (Simplified - 4 traits total)
// ============================================================================

/// Core runtime adapter trait - provides identity
pub trait RuntimeAdapter: Send + Sync + 'static {
    fn runtime_name() -> &'static str
    where
        Self: Sized;
}

/// Time operations trait - enables ctx.time() accessor
pub trait TimeOps: RuntimeAdapter {
    type Instant: Clone + Send + Sync + core::fmt::Debug + 'static;
    type Duration: Clone + Send + Sync + core::fmt::Debug + 'static;

    fn now(&self) -> Self::Instant;
    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<Self::Duration>;
    fn millis(&self, ms: u64) -> Self::Duration;
    fn secs(&self, secs: u64) -> Self::Duration;
    fn micros(&self, micros: u64) -> Self::Duration;
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send;

    /// Returns the number of whole nanoseconds in `duration`.
    ///
    /// Used by features (e.g. stage profiling) that need a numeric, runtime-agnostic
    /// representation of an elapsed [`Self::Duration`]. Implementations should saturate
    /// rather than overflow for durations larger than `u64::MAX` nanoseconds.
    fn duration_as_nanos(&self, duration: Self::Duration) -> u64;

    /// Wall-clock time as `(seconds, nanoseconds)` since the Unix epoch, if the
    /// runtime has a real-time clock.
    ///
    /// Unlike [`now`](Self::now) (a monotonic instant for measuring durations),
    /// this is an *absolute* time suitable for human/remote display — e.g. AimX
    /// `record.list` metadata timestamps.
    ///
    /// Returns `None` on platforms without a wall clock (a bare MCU with no RTC),
    /// where only monotonic time is available. The default implementation returns
    /// `None`; runtimes backed by an OS clock (or an MCU with a configured RTC)
    /// should override it.
    fn unix_time(&self) -> Option<(u64, u32)> {
        None
    }
}

/// Logging trait - enables ctx.log() accessor
pub trait Logger: RuntimeAdapter {
    fn info(&self, message: &str);
    fn debug(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
}

// ============================================================================
// Convenience Trait Bundle
// ============================================================================

/// Complete runtime trait bundle
pub trait Runtime: RuntimeAdapter + TimeOps + Logger {
    fn runtime_info(&self) -> RuntimeInfo
    where
        Self: Sized,
    {
        RuntimeInfo {
            name: Self::runtime_name(),
        }
    }
}

// Auto-implement Runtime for any type with all traits
impl<T> Runtime for T where T: RuntimeAdapter + TimeOps + Logger {}

#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub name: &'static str,
}
