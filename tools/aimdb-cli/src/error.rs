//! CLI Error Types
//!
//! Comprehensive error handling with clear, actionable error messages.

use std::fmt;
use thiserror::Error;

/// Result type for CLI operations
pub type CliResult<T> = Result<T, CliError>;

/// CLI-specific errors with helpful messages and hints
#[derive(Debug, Error)]
#[allow(dead_code)] // Some variants are for future use
pub enum CliError {
    /// Failed to connect to AimDB instance
    #[error("Connection failed: {socket}\n  Reason: {reason}\n  Hint: Check if AimDB instance is running")]
    ConnectionFailed { socket: String, reason: String },

    /// No running AimDB instances found
    #[error("No AimDB instances found\n  Searched: /tmp, /var/run/aimdb\n  Hint: Start an AimDB application with .with_remote_access()")]
    NoInstancesFound,

    /// Requested record does not exist
    #[error("Record '{name}' not found\n  Use 'aimdb record list' to see available records")]
    RecordNotFound { name: String },

    /// Attempted write operation on read-only record
    #[error("Permission denied: {operation}\n  Record '{name}' is not writable\n  Hint: Check 'writable' column in 'aimdb record list'")]
    WriteProtected { operation: String, name: String },

    /// Protocol version mismatch between client and server
    #[error("Protocol error: {message}\n  Server version: {server_version}\n  Client version: {client_version}")]
    ProtocolMismatch {
        message: String,
        server_version: String,
        client_version: String,
    },

    /// Invalid JSON provided as input
    #[error("Invalid JSON value: {input}\n  Error: {error}\n  Hint: Use single quotes around JSON: 'aimdb record set Config '{{\"key\":\"value\"}}'")]
    InvalidJson { input: String, error: String },

    /// Server returned an error response
    #[error("Server error: {code}\n  Message: {message}{}", format_details(.details))]
    ServerError {
        code: String,
        message: String,
        details: Option<serde_json::Value>,
    },

    /// I/O error occurred
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Generic error
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

fn format_details(details: &Option<serde_json::Value>) -> String {
    match details {
        Some(val) => format!("\n  Details: {}", val),
        None => String::new(),
    }
}

#[allow(dead_code)] // Helper methods for future use
impl CliError {
    /// Create a connection failed error
    pub fn connection_failed(socket: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ConnectionFailed {
            socket: socket.into(),
            reason: reason.into(),
        }
    }

    /// Create a record not found error
    pub fn record_not_found(name: impl Into<String>) -> Self {
        Self::RecordNotFound { name: name.into() }
    }

    /// Create a write protected error
    pub fn write_protected(operation: impl Into<String>, name: impl Into<String>) -> Self {
        Self::WriteProtected {
            operation: operation.into(),
            name: name.into(),
        }
    }

    /// Create a protocol mismatch error
    pub fn protocol_mismatch(
        message: impl Into<String>,
        server_version: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self::ProtocolMismatch {
            message: message.into(),
            server_version: server_version.into(),
            client_version: client_version.into(),
        }
    }

    /// Create an invalid JSON error
    pub fn invalid_json(input: impl Into<String>, error: impl fmt::Display) -> Self {
        Self::InvalidJson {
            input: input.into(),
            error: error.to_string(),
        }
    }

    /// Create a server error from protocol response
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
