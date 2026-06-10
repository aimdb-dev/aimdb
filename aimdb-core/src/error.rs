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
//! (`Io`, `Json`, sync-API timeouts) are gated on the `std` feature.
//!
//! # Error Categories
//!
//! The [`DbError`] enum covers only the errors actually used in AimDB:
//!
//! - **Network**: Connection failures, timeout errors
//! - **Buffer**: Full, lagged, or closed buffers
//! - **Outbox**: Not found, full, closed, or duplicate outbox registration
//! - **Database**: Record not found, invalid operations
//! - **Configuration**: Missing required configuration
//! - **Runtime**: Runtime adapter errors
//! - **Hardware**: MCU hardware errors (embedded only)
//! - **I/O & JSON**: Standard library integrations (std only)

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
    // ===== Network Errors (0x1000-0x1FFF) =====
    /// Connection or timeout failures
    #[error("Connection failed to {endpoint}: {reason}")]
    ConnectionFailed { endpoint: String, reason: String },

    // ===== Buffer Errors (0x2000-0x2FFF & 0xA000-0xAFFF) =====
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

    // ===== Database Errors (0x7003-0x7009) =====
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

    /// Multiple records of same type exist (ambiguous type-only lookup)
    #[error("Ambiguous type lookup: {type_name} has {count} records, use explicit key")]
    AmbiguousType { count: u32, type_name: String },

    /// Duplicate record key during registration
    #[error("Duplicate record key: {key}")]
    DuplicateRecordKey { key: String },

    /// Invalid operation attempted
    #[error("Invalid operation '{operation}': {reason}")]
    InvalidOperation { operation: String, reason: String },

    /// Permission denied for operation
    #[error("Permission denied: {operation}")]
    PermissionDenied { operation: String },

    // ===== Configuration Errors (0x4000-0x4FFF) =====
    /// Missing required configuration parameter
    #[error("Missing configuration parameter: {parameter}")]
    MissingConfiguration { parameter: String },

    /// Configuration mistakes collected by `AimDbBuilder::build()` — one
    /// entry per finding, so a single run surfaces every mistake.
    #[error("invalid configuration ({} error(s)): {}", errors.len(), join_config_errors(errors))]
    InvalidConfiguration { errors: Vec<ConfigError> },

    // ===== Runtime Errors (0x7002 & 0x5000-0x5FFF) =====
    /// Runtime execution error (task spawning, scheduling, etc.)
    #[error("Runtime error: {message}")]
    RuntimeError { message: String },

    /// Resource temporarily unavailable (used by adapters)
    #[error("Resource unavailable: {resource_name}")]
    ResourceUnavailable {
        resource_type: u8,
        resource_name: String,
    },

    // ===== Hardware Errors (0x6000-0x6FFF) - Embedded Only =====
    /// Hardware-specific errors for embedded/MCU environments
    #[error("Hardware error: component {component}, code 0x{error_code:04X}")]
    HardwareError {
        component: u8,
        error_code: u16,
        description: String,
    },

    // ===== Internal Errors (0x7001) =====
    /// Internal error for unexpected conditions
    #[error("Internal error (0x{code:04X}): {message}")]
    Internal { code: u32, message: String },

    // ===== Sync API Errors (0xB000-0xBFFF) - std only =====
    /// Failed to attach database to runtime thread
    #[cfg(feature = "std")]
    #[error("Failed to attach database: {message}")]
    AttachFailed { message: String },

    /// Failed to detach database from runtime thread
    #[cfg(feature = "std")]
    #[error("Failed to detach database: {message}")]
    DetachFailed { message: String },

    /// Timeout while setting a value
    #[cfg(feature = "std")]
    #[error("Timeout while setting value")]
    SetTimeout,

    /// Timeout while getting a value
    #[cfg(feature = "std")]
    #[error("Timeout while getting value")]
    GetTimeout,

    /// Runtime thread has shut down
    #[cfg(feature = "std")]
    #[error("Runtime thread has shut down")]
    RuntimeShutdown,

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
    // Resource type constants
    pub const RESOURCE_TYPE_MEMORY: u8 = 0;
    pub const RESOURCE_TYPE_FILE_HANDLE: u8 = 1;
    pub const RESOURCE_TYPE_SOCKET: u8 = 2;
    pub const RESOURCE_TYPE_BUFFER: u8 = 3;
    pub const RESOURCE_TYPE_THREAD: u8 = 4;
    pub const RESOURCE_TYPE_MUTEX: u8 = 5;
    pub const RESOURCE_TYPE_SEMAPHORE: u8 = 6;
    pub const RESOURCE_TYPE_CHANNEL: u8 = 7;
    pub const RESOURCE_TYPE_WOULD_BLOCK: u8 = 255;

    /// Creates a hardware error for embedded environments
    pub fn hardware_error(component: u8, error_code: u16) -> Self {
        DbError::HardwareError {
            component,
            error_code,
            description: String::new(),
        }
    }

    /// Creates an internal error with a specific error code
    pub fn internal(code: u32) -> Self {
        DbError::Internal {
            code,
            message: String::new(),
        }
    }

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

    /// Returns true if this is a network-related error
    pub fn is_network_error(&self) -> bool {
        matches!(self, DbError::ConnectionFailed { .. })
    }

    /// Returns true if this is a capacity-related error
    pub fn is_capacity_error(&self) -> bool {
        matches!(self, DbError::BufferFull { .. })
    }

    /// Returns true if this is a hardware-related error
    pub fn is_hardware_error(&self) -> bool {
        matches!(self, DbError::HardwareError { .. })
    }

    /// Returns true if this is a buffer-related error
    pub fn is_buffer_error(&self) -> bool {
        matches!(
            self,
            DbError::BufferFull { .. }
                | DbError::BufferEmpty
                | DbError::BufferLagged { .. }
                | DbError::BufferClosed { .. }
        )
    }

    /// Returns true if this is a database-related error
    pub fn is_database_error(&self) -> bool {
        matches!(
            self,
            DbError::RecordNotFound { .. }
                | DbError::RecordKeyNotFound { .. }
                | DbError::InvalidRecordId { .. }
                | DbError::TypeMismatch { .. }
                | DbError::AmbiguousType { .. }
                | DbError::DuplicateRecordKey { .. }
                | DbError::InvalidOperation { .. }
                | DbError::PermissionDenied { .. }
        )
    }

    /// Returns true if this is a configuration-related error
    pub fn is_configuration_error(&self) -> bool {
        matches!(
            self,
            DbError::MissingConfiguration { .. } | DbError::InvalidConfiguration { .. }
        )
    }

    /// Returns true if this is a runtime error
    pub fn is_runtime_error(&self) -> bool {
        matches!(
            self,
            DbError::RuntimeError { .. } | DbError::ResourceUnavailable { .. }
        )
    }

    /// Returns true if this is a transform error
    pub fn is_transform_error(&self) -> bool {
        matches!(
            self,
            DbError::CyclicDependency { .. } | DbError::TransformInputNotFound { .. }
        )
    }

    /// Returns true if this is an IO error
    #[cfg(feature = "std")]
    pub fn is_io_error(&self) -> bool {
        matches!(self, DbError::Io { .. } | DbError::IoWithContext { .. })
    }

    /// Returns true if this is a JSON error
    #[cfg(feature = "std")]
    pub fn is_json_error(&self) -> bool {
        matches!(self, DbError::Json { .. } | DbError::JsonWithContext { .. })
    }

    /// Returns a numeric error code for embedded environments
    pub const fn error_code(&self) -> u32 {
        match self {
            // Network errors: 0x1000-0x1FFF
            DbError::ConnectionFailed { .. } => 0x1002,

            // Capacity errors: 0x2000-0x2FFF
            DbError::BufferFull { .. } => 0x2002,

            // Configuration errors: 0x4000-0x4FFF
            DbError::MissingConfiguration { .. } => 0x4002,
            DbError::InvalidConfiguration { .. } => 0x4003,

            // Resource errors: 0x5000-0x5FFF
            DbError::ResourceUnavailable { .. } => 0x5002,

            // Hardware errors: 0x6000-0x6FFF
            DbError::HardwareError { .. } => 0x6001,

            // Internal errors: 0x7000-0x7FFF
            DbError::Internal { .. } => 0x7001,
            DbError::RuntimeError { .. } => 0x7002,
            DbError::RecordNotFound { .. } => 0x7003,
            DbError::InvalidOperation { .. } => 0x7004,
            DbError::PermissionDenied { .. } => 0x7005,
            DbError::RecordKeyNotFound { .. } => 0x7006,
            DbError::InvalidRecordId { .. } => 0x7007,
            DbError::TypeMismatch { .. } => 0x7008,
            DbError::AmbiguousType { .. } => 0x7009,
            DbError::DuplicateRecordKey { .. } => 0x700A,

            // Transform / Dependency Graph errors: 0xC000-0xCFFF
            DbError::CyclicDependency { .. } => 0xC001,
            DbError::TransformInputNotFound { .. } => 0xC002,

            // I/O errors: 0x8000-0x8FFF (std only)
            #[cfg(feature = "std")]
            DbError::Io { .. } => 0x8001,
            #[cfg(feature = "std")]
            DbError::IoWithContext { .. } => 0x8002,

            // JSON errors: 0x9000-0x9FFF (std only)
            #[cfg(feature = "std")]
            DbError::Json { .. } => 0x9001,
            #[cfg(feature = "std")]
            DbError::JsonWithContext { .. } => 0x9002,

            // Buffer operation errors: 0xA000-0xAFFF
            DbError::BufferLagged { .. } => 0xA001,
            DbError::BufferClosed { .. } => 0xA002,
            DbError::BufferEmpty => 0xA003,

            // Sync API errors: 0xB000-0xBFFF (std only)
            #[cfg(feature = "std")]
            DbError::AttachFailed { .. } => 0xB001,
            #[cfg(feature = "std")]
            DbError::DetachFailed { .. } => 0xB002,
            #[cfg(feature = "std")]
            DbError::SetTimeout => 0xB003,
            #[cfg(feature = "std")]
            DbError::GetTimeout => 0xB004,
            #[cfg(feature = "std")]
            DbError::RuntimeShutdown => 0xB005,
        }
    }

    /// Returns the error category (high nibble)
    pub const fn error_category(&self) -> u32 {
        self.error_code() & 0xF000
    }

    /// Helper to prepend context to a message string
    fn prepend_context<S: Into<String>>(existing: &mut String, new_context: S) {
        let new_context = new_context.into();
        existing.insert_str(0, ": ");
        existing.insert_str(0, &new_context);
    }

    /// Adds additional context to an error
    pub fn with_context<S: Into<String>>(self, context: S) -> Self {
        match self {
            DbError::ConnectionFailed {
                mut reason,
                endpoint,
            } => {
                Self::prepend_context(&mut reason, context);
                DbError::ConnectionFailed { endpoint, reason }
            }
            DbError::BufferFull {
                size,
                mut buffer_name,
            } => {
                Self::prepend_context(&mut buffer_name, context);
                DbError::BufferFull { size, buffer_name }
            }
            DbError::BufferLagged {
                lag_count,
                mut buffer_name,
            } => {
                Self::prepend_context(&mut buffer_name, context);
                DbError::BufferLagged {
                    lag_count,
                    buffer_name,
                }
            }
            DbError::BufferClosed { mut buffer_name } => {
                Self::prepend_context(&mut buffer_name, context);
                DbError::BufferClosed { buffer_name }
            }
            DbError::BufferEmpty => DbError::BufferEmpty,
            DbError::RecordNotFound { mut record_name } => {
                Self::prepend_context(&mut record_name, context);
                DbError::RecordNotFound { record_name }
            }
            DbError::RecordKeyNotFound { mut key } => {
                Self::prepend_context(&mut key, context);
                DbError::RecordKeyNotFound { key }
            }
            DbError::InvalidRecordId { id } => {
                // No context field, return as-is
                DbError::InvalidRecordId { id }
            }
            DbError::TypeMismatch {
                record_id,
                mut expected_type,
            } => {
                Self::prepend_context(&mut expected_type, context);
                DbError::TypeMismatch {
                    record_id,
                    expected_type,
                }
            }
            DbError::AmbiguousType {
                count,
                mut type_name,
            } => {
                Self::prepend_context(&mut type_name, context);
                DbError::AmbiguousType { count, type_name }
            }
            DbError::DuplicateRecordKey { mut key } => {
                Self::prepend_context(&mut key, context);
                DbError::DuplicateRecordKey { key }
            }
            DbError::InvalidOperation {
                operation,
                mut reason,
            } => {
                Self::prepend_context(&mut reason, context);
                DbError::InvalidOperation { operation, reason }
            }
            DbError::PermissionDenied { mut operation } => {
                Self::prepend_context(&mut operation, context);
                DbError::PermissionDenied { operation }
            }
            DbError::MissingConfiguration { mut parameter } => {
                Self::prepend_context(&mut parameter, context);
                DbError::MissingConfiguration { parameter }
            }
            // Collected configuration errors carry their own per-entry context
            DbError::InvalidConfiguration { .. } => self,
            DbError::RuntimeError { mut message } => {
                Self::prepend_context(&mut message, context);
                DbError::RuntimeError { message }
            }
            DbError::ResourceUnavailable {
                resource_type,
                mut resource_name,
            } => {
                Self::prepend_context(&mut resource_name, context);
                DbError::ResourceUnavailable {
                    resource_type,
                    resource_name,
                }
            }
            DbError::HardwareError {
                component,
                error_code,
                mut description,
            } => {
                Self::prepend_context(&mut description, context);
                DbError::HardwareError {
                    component,
                    error_code,
                    description,
                }
            }
            DbError::Internal { code, mut message } => {
                Self::prepend_context(&mut message, context);
                DbError::Internal { code, message }
            }
            // Sync API errors that support context (std only)
            #[cfg(feature = "std")]
            DbError::AttachFailed { mut message } => {
                Self::prepend_context(&mut message, context);
                DbError::AttachFailed { message }
            }
            #[cfg(feature = "std")]
            DbError::DetachFailed { mut message } => {
                Self::prepend_context(&mut message, context);
                DbError::DetachFailed { message }
            }
            // Sync timeout and shutdown errors don't have context fields (std only)
            #[cfg(feature = "std")]
            DbError::SetTimeout => DbError::SetTimeout,
            #[cfg(feature = "std")]
            DbError::GetTimeout => DbError::GetTimeout,
            #[cfg(feature = "std")]
            DbError::RuntimeShutdown => DbError::RuntimeShutdown,
            // Transform errors — return as-is (no mutable context field to prepend)
            DbError::CyclicDependency { .. } => self,
            DbError::TransformInputNotFound { .. } => self,
            // Convert simple I/O and JSON errors to context variants (std only)
            #[cfg(feature = "std")]
            DbError::Io { source } => DbError::IoWithContext {
                context: context.into(),
                source,
            },
            #[cfg(feature = "std")]
            DbError::Json { source } => DbError::JsonWithContext {
                context: context.into(),
                source,
            },
            // Prepend to existing context for context variants (std only)
            #[cfg(feature = "std")]
            DbError::IoWithContext {
                context: mut ctx,
                source,
            } => {
                Self::prepend_context(&mut ctx, context);
                DbError::IoWithContext {
                    context: ctx,
                    source,
                }
            }
            #[cfg(feature = "std")]
            DbError::JsonWithContext {
                context: mut ctx,
                source,
            } => {
                Self::prepend_context(&mut ctx, context);
                DbError::JsonWithContext {
                    context: ctx,
                    source,
                }
            }
        }
    }

    /// Converts this error into an anyhow::Error (std only)
    #[cfg(feature = "std")]
    pub fn into_anyhow(self) -> anyhow::Error {
        self.into()
    }
}

/// Type alias for Results using DbError
pub type DbResult<T> = Result<T, DbError>;

// ============================================================================
// Error Conversions
// ============================================================================

/// Convert executor errors to database errors
///
/// This allows runtime adapters to return `ExecutorError` while the core
/// database works with `DbError` for consistency across the API.
impl From<aimdb_executor::ExecutorError> for DbError {
    fn from(err: aimdb_executor::ExecutorError) -> Self {
        use aimdb_executor::ExecutorError;

        // `ExecutorError`'s `message` field is `String` on std and
        // `&'static str` on no_std; `.into()` is required for the latter.
        #[allow(clippy::useless_conversion)]
        match err {
            ExecutorError::RuntimeUnavailable { message }
            | ExecutorError::TaskJoinFailed { message } => DbError::RuntimeError {
                message: message.into(),
            },
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
    fn test_invalid_configuration_display_and_code() {
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
        assert_eq!(err.error_code(), 0x4003);
        assert_eq!(err.error_category(), 0x4000);
        assert!(err.is_configuration_error());

        let s = err.to_string();
        assert!(s.contains("2 error(s)"), "got: {s}");
        assert!(
            s.contains("record 'rec.a' (mqtt://broker/a): missing serializer"),
            "got: {s}"
        );
        assert!(s.contains("record 'rec.b': duplicate producer"), "got: {s}");
    }

    #[test]
    fn test_error_codes() {
        let connection_error = DbError::ConnectionFailed {
            endpoint: "localhost".to_string(),
            reason: "timeout".to_string(),
        };
        assert_eq!(connection_error.error_code(), 0x1002);
        assert_eq!(connection_error.error_category(), 0x1000);

        let buffer_error = DbError::BufferFull {
            size: 1024,
            buffer_name: String::new(),
        };
        assert_eq!(buffer_error.error_code(), 0x2002);
    }

    #[test]
    fn test_helper_methods() {
        let connection_error = DbError::ConnectionFailed {
            endpoint: "localhost".to_string(),
            reason: "timeout".to_string(),
        };

        assert!(connection_error.is_network_error());
        assert!(!connection_error.is_capacity_error());
        assert!(!connection_error.is_hardware_error());

        let hardware_error = DbError::hardware_error(2, 404);
        assert!(hardware_error.is_hardware_error());

        let internal_error = DbError::internal(500);
        assert!(matches!(
            internal_error,
            DbError::Internal { code: 500, .. }
        ));
    }

    #[test]
    fn test_error_context() {
        let error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "timeout".to_string(),
        }
        .with_context("Database connection")
        .with_context("Application startup");

        if let DbError::ConnectionFailed { reason, .. } = error {
            assert_eq!(reason, "Application startup: Database connection: timeout");
        } else {
            panic!("Expected ConnectionFailed");
        }
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

    #[test]
    fn test_buffer_empty() {
        let buffer = DbError::BufferEmpty;
        assert!(buffer.is_buffer_error());
        assert!(!buffer.is_network_error());
    }

    #[test]
    fn test_configuration_error() {
        let configuration = DbError::MissingConfiguration {
            parameter: "my_parameter".to_string(),
        };
        assert!(configuration.is_configuration_error());
        assert!(!configuration.is_buffer_error());
    }

    #[test]
    fn test_runtime_error() {
        let runtime = DbError::RuntimeError {
            message: "Hello, World".to_string(),
        };
        assert!(runtime.is_runtime_error());
        assert!(!runtime.is_buffer_error());
    }

    #[test]
    fn test_database_error() {
        let database = DbError::InvalidRecordId { id: 5 };
        assert!(database.is_database_error());
        assert!(!database.is_buffer_error());
    }

    #[test]
    fn test_transform_error() {
        let transform = DbError::CyclicDependency {
            records: vec!["Hello, World!".to_string()],
        };
        assert!(transform.is_transform_error());
        assert!(!transform.is_buffer_error());
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_io_error() {
        let io_error = std::io::Error::other("test");
        let db_error: DbError = io_error.into();
        assert!(db_error.is_io_error());
        assert!(!db_error.is_buffer_error());
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_json_error() {
        let json_error = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let db_error: DbError = json_error.into();
        assert!(db_error.is_json_error());
        assert!(!db_error.is_buffer_error());
    }
}
