//! Errors of the blocking facade.
use alloc::string::String;

use aimdb_core::{DbError, DbErrorKind};

/// Errors from the synchronous (blocking) API.
///
/// Facade-specific failures (attach/detach, runtime-thread shutdown) are their
/// own variants; anything from the underlying database wraps a [`DbError`]
/// via [`SyncError::Db`].
///
/// `#[non_exhaustive]`: match on [`SyncError::kind`] where you only need to
/// know what to do about the failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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

    /// This handle, producer or consumer was created before a `fork()`, and
    /// this is the child. The runtime thread it needs did not survive.
    #[error("created before a fork(); this process has no runtime thread for it")]
    ForkedChild,

    /// Error from the underlying database.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl SyncError {
    /// Classify the failure by what the caller can do about it.
    ///
    /// Returns [`DbErrorKind`] rather than a kind of its own, so a caller — an
    /// FFI layer above all — has one set of actions for the whole stack instead
    /// of one per crate. [`Db`](SyncError::Db) delegates, so a buffer that is
    /// merely empty classifies the same whether it is reached through this
    /// facade or through `aimdb-core` directly.
    pub fn kind(&self) -> DbErrorKind {
        match self {
            // The cause may well have been a configuration mistake — the
            // message says so — but these carry a `String`, so its kind does
            // not survive the trip.
            Self::AttachFailed { .. } | Self::DetachFailed { .. } => DbErrorKind::Internal,

            Self::SetTimeout | Self::GetTimeout => DbErrorKind::Retry,

            // Terminal for the same reason RuntimeShutdown is: the runtime
            // thread is gone and will not come back in this process.
            Self::RuntimeShutdown | Self::ForkedChild => DbErrorKind::Closed,

            Self::Db(err) => err.kind(),
        }
    }
}

/// Result alias for blocking-facade operations.
pub type SyncResult<T> = Result<T, SyncError>;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn facade_failures_classify_by_action() {
        assert_eq!(
            SyncError::AttachFailed {
                message: "m".to_string()
            }
            .kind(),
            DbErrorKind::Internal
        );
        assert_eq!(SyncError::GetTimeout.kind(), DbErrorKind::Retry);
        assert_eq!(SyncError::SetTimeout.kind(), DbErrorKind::Retry);
        assert_eq!(SyncError::RuntimeShutdown.kind(), DbErrorKind::Closed);
    }

    /// The point of returning `DbErrorKind` rather than a kind of this crate's
    /// own: one switch covers the whole stack.
    #[test]
    fn a_wrapped_db_error_keeps_its_kind() {
        for err in [
            DbError::BufferEmpty,
            DbError::BufferClosed {
                buffer_name: "b".to_string(),
            },
            DbError::missing_configuration("p"),
        ] {
            let expected = err.kind();
            assert_eq!(SyncError::Db(err).kind(), expected);
        }
    }
}
