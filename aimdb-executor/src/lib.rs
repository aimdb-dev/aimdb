//! AimDB Executor Traits
//!
//! Pure trait definitions for async execution across different runtime environments.
//! Enables dependency inversion where the core database depends on abstractions
//! rather than concrete runtime implementations.
//!
//! # Design Philosophy
//!
//! - **Runtime Agnostic**: No concrete runtime dependencies
//! - **Simple Trait Structure**: 4 focused traits covering all runtime needs
//! - **Platform Flexible**: Works across std and no_std environments
//! - **Zero Dependencies**: Pure trait definitions with minimal coupling
//!
//! # Trait Structure
//!
//! 1. **`RuntimeAdapter`** - Platform identity and metadata
//! 2. **`TimeOps`** - Time operations (now, sleep, duration helpers)
//! 3. **`Logger`** - Structured logging (info, debug, warn, error)
//! 4. **`Spawn`** - Task spawning with platform-specific tokens

#![cfg_attr(not(feature = "std"), no_std)]

use core::future::Future;

// ============================================================================
// Error Types
// ============================================================================

pub type ExecutorResult<T> = Result<T, ExecutorError>;

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
}

/// Logging trait - enables ctx.log() accessor
pub trait Logger: RuntimeAdapter {
    fn info(&self, message: &str);
    fn debug(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
}

/// Task spawning trait - adapter-specific implementation
pub trait Spawn: RuntimeAdapter {
    type SpawnToken: Send + 'static;
    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static;
}

// ============================================================================
// Convenience Trait Bundle
// ============================================================================

/// Complete runtime trait bundle
pub trait Runtime: RuntimeAdapter + TimeOps + Logger + Spawn {
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
impl<T> Runtime for T where T: RuntimeAdapter + TimeOps + Logger + Spawn {}

#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub name: &'static str,
}
