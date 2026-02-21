//! Persistence error types for AimDB.

use core::fmt;

/// Errors that can occur during persistence operations.
#[derive(Debug)]
pub enum PersistenceError {
    /// The persistence backend is not configured.
    /// Returned when calling `.query_latest()` / `.query_range()` on an `AimDb`
    /// that was built without `.with_persistence()`.
    NotConfigured,

    /// The backend writer thread has shut down (all senders dropped).
    BackendShutdown,

    /// A backend-specific error (e.g. SQLite, Postgres).
    Backend(String),

    /// JSON serialization/deserialization error.
    Serialization(String),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistenceError::NotConfigured => {
                write!(
                    f,
                    "persistence backend not configured (call .with_persistence() on the builder)"
                )
            }
            PersistenceError::BackendShutdown => {
                write!(f, "persistence backend has shut down")
            }
            PersistenceError::Backend(msg) => write!(f, "persistence backend error: {}", msg),
            PersistenceError::Serialization(msg) => {
                write!(f, "persistence serialization error: {}", msg)
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<serde_json::Error> for PersistenceError {
    fn from(e: serde_json::Error) -> Self {
        PersistenceError::Serialization(e.to_string())
    }
}
