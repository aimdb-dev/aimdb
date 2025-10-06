//! Runtime adapter integration for AimDB
//!
//! This module provides integration with the aimdb-executor trait system,
//! adapting executor errors to database errors and re-exporting key traits.

// Re-export executor traits for convenience
pub use aimdb_executor::{
    AimDbService, CommonRuntimeTraits, DelayCapableAdapter, DynamicRuntimeTraits, ExecutorError,
    ExecutorResult, RuntimeAdapter, RuntimeInfo, SpawnDynamically, SpawnStatically,
    StaticRuntimeTraits,
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
