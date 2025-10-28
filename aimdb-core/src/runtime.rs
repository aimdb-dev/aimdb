//! Runtime adapter traits and implementations for AimDB
//!
//! This module re-exports the runtime traits from aimdb-executor for convenience.
//! All runtime adapters implement these traits to provide platform-specific
//! execution, timing, and logging capabilities.

// Re-export simplified executor traits
pub use aimdb_executor::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Sleeper, Spawn,
    TimeOps, TimeSource,
};
