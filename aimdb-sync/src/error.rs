//! Errors of the blocking facade.
use alloc::string::String;

use aimdb_core::DbError;

/// Errors from the synchronous (blocking) API.
///
/// Facade-specific failures (attach/detach, channel timeouts, runtime-thread
/// shutdown) are their own variants; anything from the underlying database
/// wraps a [`DbError`] via [`SyncError::Db`].
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Failed to attach the database to the runtime thread.
    #[error("Failed to attach database: {message}")]
    AttachFailed {
        /// Human-readable description of the failure.
        message: String,
    },

    /// Failed to detach the database from the runtime thread.
    #[error("Failed to detach database: {message}")]
    DetachFailed {
        /// Human-readable description of the failure.
        message: String,
    },

    /// Timeout while setting a value.
    #[error("Timeout while setting value")]
    SetTimeout,

    /// Timeout while getting a value.
    #[error("Timeout while getting value")]
    GetTimeout,

    /// The runtime thread has shut down.
    #[error("Runtime thread has shut down")]
    RuntimeShutdown,

    /// Error from the underlying database.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Result alias for blocking-facade operations.
pub type SyncResult<T> = Result<T, SyncError>;
