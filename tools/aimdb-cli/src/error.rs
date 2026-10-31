//! CLI Error Types
//!
//! Comprehensive error handling with clear, actionable error messages.

use std::fmt;
use thiserror::Error;

/// Result type for CLI operations
pub type CliResult<T> = Result<T, CliError>;

/// Exit code: the instance or a remote service refused the request.
pub const EXIT_REJECTED: i32 = 1;
/// Exit code: bad arguments or malformed input (also clap's usage errors).
pub const EXIT_USAGE: i32 = 2;
/// Exit code: network or TLS failure, unreachable instance, incompatible
/// peer, I/O. Worth retrying.
pub const EXIT_ENVIRONMENT: i32 = 3;

/// CLI-specific errors with helpful messages and hints
#[derive(Debug, Error)]
#[allow(dead_code)] // Some variants are for future use
pub enum CliError {
    /// Failed to connect to AimDB instance
    #[error("Connection failed: {endpoint}\n  Reason: {reason}\n  Hint: Check if AimDB instance is running")]
    ConnectionFailed { endpoint: String, reason: String },

    /// Endpoint string was malformed or named an unsupported/not-built-in scheme
    #[error("Unsupported endpoint: {endpoint}\n  Reason: {reason}\n  Hint: use unix://PATH, serial://DEVICE?baud=N, or a bare path")]
    UnsupportedEndpoint { endpoint: String, reason: String },

    /// No running AimDB instances found
    #[error("No AimDB instances found\n  Searched: /tmp, /var/run/aimdb\n  Hint: Start an AimDB application that registers a remote-access server (e.g. UdsServer via .with_connector(...))")]
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
    #[error("Server error: {code}\n  Message: {message}{}", .details.as_ref().map(|d| format!("\n  Details: {d}")).unwrap_or_default())]
    ServerError {
        code: String,
        message: String,
        details: Option<serde_json::Value>,
    },

    /// The server's reply broke the AimX protocol (e.g. a value without `ts`)
    #[error("Protocol error: {reason}\n  Hint: the server must speak AimX 4.x")]
    Protocol { reason: String },

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

impl From<aimdb_client::ClientError> for CliError {
    fn from(err: aimdb_client::ClientError) -> Self {
        match err {
            aimdb_client::ClientError::NoInstancesFound => CliError::NoInstancesFound,
            aimdb_client::ClientError::ConnectionFailed { endpoint, reason } => {
                CliError::ConnectionFailed { endpoint, reason }
            }
            aimdb_client::ClientError::UnsupportedEndpoint { endpoint, reason } => {
                CliError::UnsupportedEndpoint { endpoint, reason }
            }
            aimdb_client::ClientError::ServerError {
                code,
                message,
                details,
            } => CliError::ServerError {
                code,
                message,
                details,
            },
            aimdb_client::ClientError::Protocol { reason } => CliError::Protocol { reason },
            aimdb_client::ClientError::Io(e) => CliError::Io(e),
            aimdb_client::ClientError::Json(e) => CliError::Json(e),
            aimdb_client::ClientError::Other(e) => CliError::Other(e),
        }
    }
}

impl CliError {
    /// Process exit code for this error's failure class, so scripts can tell
    /// a retryable failure from a rejection. Codes 4 and up are left to
    /// individual commands.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ConnectionFailed { .. }
            | Self::NoInstancesFound
            | Self::ProtocolMismatch { .. }
            | Self::Protocol { .. }
            | Self::Io(_)
            // A reply or file that does not parse.
            | Self::Json(_) => EXIT_ENVIRONMENT,
            // `InvalidJson` is JSON given on the command line.
            Self::UnsupportedEndpoint { .. } | Self::InvalidJson { .. } => EXIT_USAGE,
            Self::RecordNotFound { .. }
            | Self::WriteProtected { .. }
            | Self::ServerError { .. }
            | Self::Other(_) => EXIT_REJECTED,
        }
    }
}

#[allow(dead_code)] // Helper methods for future use
impl CliError {
    /// Create a connection failed error
    pub fn connection_failed(endpoint: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ConnectionFailed {
            endpoint: endpoint.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_failure_classes() {
        let environment = [
            CliError::connection_failed("unix:///x", "refused"),
            CliError::NoInstancesFound,
            CliError::protocol_mismatch("m", "2", "1"),
            CliError::Io(std::io::Error::other("io")),
            CliError::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err()),
        ];
        let usage = [
            CliError::UnsupportedEndpoint {
                endpoint: "bogus://x".into(),
                reason: "unknown scheme".into(),
            },
            CliError::invalid_json("{", "eof"),
        ];
        let rejected = [
            CliError::record_not_found("r"),
            CliError::write_protected("set", "r"),
            CliError::server_error("denied", "no", None),
            CliError::Other(anyhow::anyhow!("other")),
        ];

        for e in &environment {
            assert_eq!(e.exit_code(), EXIT_ENVIRONMENT, "{e:?}");
        }
        for e in &usage {
            assert_eq!(e.exit_code(), EXIT_USAGE, "{e:?}");
        }
        for e in &rejected {
            assert_eq!(e.exit_code(), EXIT_REJECTED, "{e:?}");
        }
    }
}
