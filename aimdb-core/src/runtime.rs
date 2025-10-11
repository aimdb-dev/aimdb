//! Runtime adapter traits and implementations for AimDB
//!
//! This module re-exports the runtime traits from aimdb-executor and provides
//! additional runtime-related functionality for the core database.

// Re-export simplified executor traits
pub use aimdb_executor::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Sleeper, Spawn,
    TimeOps, TimeSource,
};

/// Convert executor errors to database errors
///
/// This allows adapters to return `ExecutorError` while the core database
/// works with `DbError` for consistency with the rest of the API.
impl From<ExecutorError> for crate::DbError {
    fn from(err: ExecutorError) -> Self {
        match err {
            ExecutorError::SpawnFailed { message } => {
                #[cfg(feature = "std")]
                {
                    crate::DbError::RuntimeError { message }
                }
                #[cfg(not(feature = "std"))]
                {
                    let _ = message; // Use the message variable to avoid unused warnings
                    crate::DbError::RuntimeError { _message: () }
                }
            }
            ExecutorError::RuntimeUnavailable { message } => {
                #[cfg(feature = "std")]
                {
                    crate::DbError::RuntimeError { message }
                }
                #[cfg(not(feature = "std"))]
                {
                    let _ = message; // Use the message variable to avoid unused warnings
                    crate::DbError::RuntimeError { _message: () }
                }
            }
            ExecutorError::TaskJoinFailed { message } => {
                #[cfg(feature = "std")]
                {
                    crate::DbError::RuntimeError { message }
                }
                #[cfg(not(feature = "std"))]
                {
                    let _ = message; // Use the message variable to avoid unused warnings
                    crate::DbError::RuntimeError { _message: () }
                }
            }
        }
    }
}
