//! Error types for AimDB client library

use thiserror::Error;

/// Result type for client operations
pub type ClientResult<T> = Result<T, ClientError>;

/// Errors that can occur during client operations
#[derive(Error, Debug)]
pub enum ClientError {
    /// No AimDB instances found during discovery
    #[error("No running AimDB instances found")]
    NoInstancesFound,

    /// Connection error
    #[error("Connection failed to {socket}: {reason}")]
    ConnectionFailed { socket: String, reason: String },

    /// Server returned an error
    #[error("Server error (code {code}): {message}")]
    ServerError {
        code: String,
        message: String,
        details: Option<serde_json::Value>,
    },

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Generic error
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl ClientError {
    /// Create a connection failed error
    pub fn connection_failed(socket: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ConnectionFailed {
            socket: socket.into(),
            reason: reason.into(),
        }
    }

    /// Create a server error
    pub fn server_error(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        Self::ServerError {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}
