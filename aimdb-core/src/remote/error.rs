//! Error types for AimX remote access protocol

use std::string::String;
use thiserror::Error;

/// Error type for remote access operations
#[derive(Debug, Clone, Error)]
pub enum RemoteError {
    /// Malformed message or invalid JSON
    #[error("Protocol error: {message}")]
    ProtocolError { message: String },

    /// Incompatible protocol versions
    #[error("Version mismatch: client {client_version}, server {server_version}")]
    VersionMismatch {
        client_version: String,
        server_version: String,
    },

    /// Record or subscription not found
    #[error("Not found: {resource}")]
    NotFound { resource: String },

    /// Operation not permitted
    #[error("Permission denied: {reason}")]
    PermissionDenied { reason: String },

    /// Subscription queue overflow
    #[error("Queue full: {queue_name}")]
    QueueFull { queue_name: String },

    /// Server internal error
    #[error("Internal error: {message}")]
    InternalError { message: String },

    /// Too many subscriptions for this client
    #[error("Too many subscriptions (limit: {limit})")]
    TooManySubscriptions { limit: usize },

    /// Record has no current value
    #[error("No value: {record_name}")]
    NoValue { record_name: String },

    /// Record has no buffer configured
    #[error("No buffer: {record_name}")]
    NoBuffer { record_name: String },

    /// Invalid parameter or value
    #[error("Validation error: {message}")]
    ValidationError { message: String },

    /// Authentication token required
    #[error("Authentication required")]
    AuthRequired,

    /// Invalid authentication token
    #[error("Authentication failed")]
    AuthFailed,
}

impl RemoteError {
    /// Returns the protocol error code
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProtocolError { .. } => "PROTOCOL_ERROR",
            Self::VersionMismatch { .. } => "VERSION_MISMATCH",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::PermissionDenied { .. } => "PERMISSION_DENIED",
            Self::QueueFull { .. } => "QUEUE_FULL",
            Self::InternalError { .. } => "INTERNAL_ERROR",
            Self::TooManySubscriptions { .. } => "TOO_MANY_SUBSCRIPTIONS",
            Self::NoValue { .. } => "NO_VALUE",
            Self::NoBuffer { .. } => "NO_BUFFER",
            Self::ValidationError { .. } => "VALIDATION_ERROR",
            Self::AuthRequired => "AUTH_REQUIRED",
            Self::AuthFailed => "AUTH_FAILED",
        }
    }

    /// Returns whether this error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::NotFound { .. }
                | Self::QueueFull { .. }
                | Self::InternalError { .. }
                | Self::NoValue { .. }
        )
    }
}

/// Result type for remote operations
pub type RemoteResult<T> = Result<T, RemoteError>;

// Conversion from DbError to RemoteError
impl From<crate::DbError> for RemoteError {
    fn from(err: crate::DbError) -> Self {
        use crate::DbError;
        match err {
            DbError::RecordNotFound { record_name } => RemoteError::NotFound {
                resource: format!("record '{}'", record_name),
            },
            DbError::InvalidOperation { operation, reason } => RemoteError::ValidationError {
                message: format!("{}: {}", operation, reason),
            },
            DbError::BufferFull { buffer_name, .. } => RemoteError::QueueFull {
                queue_name: buffer_name,
            },
            DbError::PermissionDenied { operation } => {
                RemoteError::PermissionDenied { reason: operation }
            }
            _ => RemoteError::InternalError {
                message: err.to_string(),
            },
        }
    }
}

impl From<std::io::Error> for RemoteError {
    fn from(err: std::io::Error) -> Self {
        RemoteError::InternalError {
            message: format!("I/O error: {}", err),
        }
    }
}

impl From<serde_json::Error> for RemoteError {
    fn from(err: serde_json::Error) -> Self {
        RemoteError::ProtocolError {
            message: format!("JSON error: {}", err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(
            RemoteError::ProtocolError {
                message: "test".to_string(),
            }
            .code(),
            "PROTOCOL_ERROR"
        );

        assert_eq!(
            RemoteError::NotFound {
                resource: "test".to_string(),
            }
            .code(),
            "NOT_FOUND"
        );

        assert_eq!(
            RemoteError::PermissionDenied {
                reason: "test".to_string(),
            }
            .code(),
            "PERMISSION_DENIED"
        );
    }

    #[test]
    fn test_retryable() {
        assert!(RemoteError::NotFound {
            resource: "test".to_string(),
        }
        .is_retryable());

        assert!(!RemoteError::PermissionDenied {
            reason: "test".to_string(),
        }
        .is_retryable());
    }
}
