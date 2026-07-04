//! Error handling for AimDB core operations
//!
//! This module provides a streamlined error type system that works across all AimDB
//! target platforms: MCU (no_std), edge devices and cloud environments.
//!
//! # Platform Compatibility
//!
//! Every variant has a single shape on all targets: context fields use
//! `alloc::string::String`, which is available everywhere (the crate
//! unconditionally requires `alloc`), and `Display`/`Error` come from
//! `thiserror` (no_std-capable since 2.x). Only variants that wrap std types
//! (`Io`, `Json`) are gated on the `std` feature.
//!
//! # Error Categories
//!
//! The [`DbError`] enum covers only the errors actually constructed by AimDB:
//!
//! - **Network**: Connection failures, timeout errors
//! - **Buffer**: Full, lagged, closed, or empty buffers
//! - **Database**: Record not found, invalid operations
//! - **Configuration**: Missing required configuration; mistakes collected by `build()`
//! - **Runtime**: Runtime adapter errors
//! - **Transform**: Dependency-graph cycles, missing transform inputs
//! - **I/O & JSON**: Standard library integrations (std only)
//!
//! Layer-specific errors live with their layer: the blocking facade has
//! `aimdb_sync::SyncError`, connectors have their own error types wrapping
//! `DbError` where needed.

use alloc::string::String;
use alloc::vec::Vec;
use thiserror::Error;

#[cfg(feature = "std")]
use std::io;

/// One configuration mistake found while configuring an `AimDbBuilder`.
///
/// Builder methods never panic on user mistakes; they record one of these and
/// `build()` reports **all** of them at once via
/// [`DbError::InvalidConfiguration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Key of the record the mistake belongs to. Errors recorded before the
    /// key is known (inside `TypedRecord` setters) carry an empty key that
    /// `build()` fills in when draining.
    pub record_key: String,
    /// Connector URL, for link-related mistakes.
    pub url: Option<String>,
    /// Human-readable description of the mistake.
    pub message: String,
}

impl ConfigError {
    /// Builds a `ConfigError`.
    pub fn new(
        record_key: impl Into<String>,
        url: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            record_key: record_key.into(),
            url,
            message: message.into(),
        }
    }
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Graph-level findings (cycles) span records and carry no single key.
        if !self.record_key.is_empty() {
            write!(f, "record '{}'", self.record_key)?;
            if let Some(url) = &self.url {
                write!(f, " ({url})")?;
            }
            write!(f, ": ")?;
        } else if let Some(url) = &self.url {
            write!(f, "({url}): ")?;
        }
        write!(f, "{}", self.message)
    }
}

/// Joins collected configuration errors for `InvalidConfiguration`'s `Display`.
fn join_config_errors(errors: &[ConfigError]) -> String {
    use core::fmt::Write;
    let mut out = String::new();
    for (i, e) in errors.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let _ = write!(out, "{e}");
    }
    out
}

/// Streamlined error type for AimDB operations
///
/// Only includes errors that are actually used in the codebase,
/// removing theoretical/unused error variants for simplicity.
#[derive(Debug, Error)]
pub enum DbError {
    // ===== Network Errors =====
    /// Connection or timeout failures
    #[error("Connection failed to {endpoint}: {reason}")]
    ConnectionFailed { endpoint: String, reason: String },

    // ===== Buffer Errors =====
    /// Buffer is full and cannot accept more items
    #[error("Buffer full: {buffer_name} ({size} items)")]
    BufferFull { size: u32, buffer_name: String },

    /// Consumer lagged behind producer (SPMC ring buffers)
    #[error("Consumer lagged by {lag_count} messages")]
    BufferLagged { lag_count: u64, buffer_name: String },

    /// Buffer channel has been closed (shutdown)
    #[error("Buffer channel closed: {buffer_name}")]
    BufferClosed { buffer_name: String },

    /// Non-blocking receive found no pending values
    #[error("Buffer empty: no pending values")]
    BufferEmpty,

    // ===== Database Errors =====
    /// Record type not found in database (legacy, by type name)
    #[error("Record type not found: {record_name}")]
    RecordNotFound { record_name: String },

    /// Record key not found in registry
    #[error("Record key not found: {key}")]
    RecordKeyNotFound { key: String },

    /// RecordId out of bounds or invalid
    #[error("Invalid record ID: {id}")]
    InvalidRecordId { id: u32 },

    /// Type mismatch when accessing record by ID
    #[error("Type mismatch: expected {expected_type}, record {record_id} has different type")]
    TypeMismatch {
        record_id: u32,
        expected_type: String,
    },

    /// Invalid operation attempted
    #[error("Invalid operation '{operation}': {reason}")]
    InvalidOperation { operation: String, reason: String },

    /// Permission denied for operation
    #[error("Permission denied: {operation}")]
    PermissionDenied { operation: String },

    // ===== Configuration Errors =====
    /// Missing required configuration parameter
    #[error("Missing configuration parameter: {parameter}")]
    MissingConfiguration { parameter: String },

    /// Configuration mistakes collected by `AimDbBuilder::build()` — one
    /// entry per finding, so a single run surfaces every mistake.
    #[error("invalid configuration ({} error(s)): {}", errors.len(), join_config_errors(errors))]
    InvalidConfiguration { errors: Vec<ConfigError> },

    // ===== Runtime Errors =====
    /// Runtime execution error (task spawning, scheduling, etc.)
    #[error("Runtime error: {message}")]
    RuntimeError { message: String },

    // ===== Internal Errors =====
    /// Internal error for unexpected conditions
    #[error("Internal error (0x{code:04X}): {message}")]
    Internal { code: u32, message: String },

    // ===== Transform / Dependency Graph Errors =====
    /// Transform dependency graph contains a cycle
    #[error("Cyclic dependency detected among records: {records:?}")]
    CyclicDependency { records: Vec<String> },

    /// Transform input key references a record that was not registered
    #[error("Transform on '{output_key}' references unregistered input '{input_key}'")]
    TransformInputNotFound {
        output_key: String,
        input_key: String,
    },

    // ===== Standard Library Integrations (std only) =====
    /// I/O operation error
    #[cfg(feature = "std")]
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },

    /// I/O operation error with context
    #[cfg(feature = "std")]
    #[error("I/O error: {context}: {source}")]
    IoWithContext {
        context: String,
        #[source]
        source: io::Error,
    },

    /// JSON serialization error
    #[cfg(feature = "std")]
    #[error("JSON error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },

    /// JSON serialization error with context
    #[cfg(feature = "std")]
    #[error("JSON error: {context}: {source}")]
    JsonWithContext {
        context: String,
        #[source]
        source: serde_json::Error,
    },
}

// ===== DbError Implementation =====
impl DbError {
    /// Builds a [`RuntimeError`](DbError::RuntimeError).
    pub fn runtime_error(message: impl Into<String>) -> Self {
        DbError::RuntimeError {
            message: message.into(),
        }
    }

    /// Builds a [`PermissionDenied`](DbError::PermissionDenied).
    pub fn permission_denied(operation: impl Into<String>) -> Self {
        DbError::PermissionDenied {
            operation: operation.into(),
        }
    }

    /// Builds a [`RecordKeyNotFound`](DbError::RecordKeyNotFound).
    pub fn record_key_not_found(key: impl Into<String>) -> Self {
        DbError::RecordKeyNotFound { key: key.into() }
    }

    /// Builds a [`MissingConfiguration`](DbError::MissingConfiguration).
    pub fn missing_configuration(parameter: impl Into<String>) -> Self {
        DbError::MissingConfiguration {
            parameter: parameter.into(),
        }
    }
}

/// Type alias for Results using DbError
pub type DbResult<T> = Result<T, DbError>;

// ============================================================================
// Error Conversions
// ============================================================================

/// Convert executor errors to database errors
///
/// This allows engine plumbing to return `ExecutorError` while the core
/// database works with `DbError` for consistency across the API.
impl From<crate::executor::ExecutorError> for DbError {
    fn from(err: crate::executor::ExecutorError) -> Self {
        use crate::executor::ExecutorError;

        match err {
            ExecutorError::QueueClosed => DbError::runtime_error("join queue closed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn test_error_size_constraint() {
        let size = core::mem::size_of::<DbError>();
        assert!(
            size <= 64,
            "DbError size ({} bytes) exceeds 64-byte embedded limit",
            size
        );
    }

    #[test]
    fn test_invalid_configuration_display() {
        let err = DbError::InvalidConfiguration {
            errors: vec![
                ConfigError::new(
                    "rec.a",
                    Some("mqtt://broker/a".to_string()),
                    "missing serializer",
                ),
                ConfigError::new("rec.b", None, "duplicate producer"),
            ],
        };

        let s = err.to_string();
        assert!(s.contains("2 error(s)"), "got: {s}");
        assert!(
            s.contains("record 'rec.a' (mqtt://broker/a): missing serializer"),
            "got: {s}"
        );
        assert!(s.contains("record 'rec.b': duplicate producer"), "got: {s}");
    }

    #[test]
    fn test_helper_constructors() {
        assert!(matches!(
            DbError::runtime_error("boom"),
            DbError::RuntimeError { .. }
        ));
        assert!(matches!(
            DbError::permission_denied("write"),
            DbError::PermissionDenied { .. }
        ));
        assert!(matches!(
            DbError::record_key_not_found("k"),
            DbError::RecordKeyNotFound { .. }
        ));
        assert!(matches!(
            DbError::missing_configuration("p"),
            DbError::MissingConfiguration { .. }
        ));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_io_json_conversions() {
        let io_error = std::io::Error::other("File not found");
        let db_error: DbError = io_error.into();
        assert!(matches!(db_error, DbError::Io { .. }));

        let json_error = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let db_error: DbError = json_error.into();
        assert!(matches!(db_error, DbError::Json { .. }));
    }
}
