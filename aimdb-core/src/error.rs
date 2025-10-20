//! Error handling for AimDB core operations
//!
//! This module provides a streamlined error type system that works across all AimDB
//! target platforms: MCU (no_std), edge devices and cloud environments.
//!
//! # Platform Compatibility
//!
//! The error system is designed with conditional compilation to optimize for
//! different deployment targets:
//!
//! - **MCU/Embedded**: Minimal memory footprint with `no_std` compatibility
//! - **Edge/Desktop**: Rich error context with standard library features  
//! - **Cloud**: Full error chains and debugging capabilities with thiserror integration
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

#[cfg(feature = "std")]
use thiserror::Error;

#[cfg(feature = "std")]
use std::io;

/// Streamlined error type for AimDB operations
///
/// Only includes errors that are actually used in the codebase,
/// removing theoretical/unused error variants for simplicity.
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum DbError {
    // ===== Network Errors (0x1000-0x1FFF) =====
    /// Connection or timeout failures
    #[cfg_attr(feature = "std", error("Connection failed to {endpoint}: {reason}"))]
    ConnectionFailed {
        #[cfg(feature = "std")]
        endpoint: String,
        #[cfg(feature = "std")]
        reason: String,
        #[cfg(not(feature = "std"))]
        _endpoint: (),
        #[cfg(not(feature = "std"))]
        _reason: (),
    },

    // ===== Buffer Errors (0x2000-0x2FFF & 0xA000-0xAFFF) =====
    /// Buffer is full and cannot accept more items
    #[cfg_attr(feature = "std", error("Buffer full: {buffer_name} ({size} items)"))]
    BufferFull {
        size: u32,
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },

    /// Consumer lagged behind producer (SPMC ring buffers)
    #[cfg_attr(feature = "std", error("Consumer lagged by {lag_count} messages"))]
    BufferLagged {
        lag_count: u64,
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },

    /// Buffer channel has been closed (shutdown)
    #[cfg_attr(feature = "std", error("Buffer channel closed: {buffer_name}"))]
    BufferClosed {
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },

    // ===== Outbox Errors (0xA000-0xAFFF) =====
    /// Outbox not found for the specified type
    #[cfg_attr(feature = "std", error("Outbox not found for type: {type_name}"))]
    OutboxNotFound {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Outbox channel is full
    #[cfg_attr(
        feature = "std",
        error("Outbox channel full: {type_name} (capacity: {capacity})")
    )]
    OutboxFull {
        capacity: usize,
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Outbox channel has been closed
    #[cfg_attr(feature = "std", error("Outbox channel closed: {type_name}"))]
    OutboxClosed {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Outbox already exists for this type
    #[cfg_attr(feature = "std", error("Outbox already exists for type: {type_name}"))]
    OutboxAlreadyExists {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    // ===== Database Errors (0x7003-0x7004) =====
    /// Record type not found in database
    #[cfg_attr(feature = "std", error("Record type not found: {record_name}"))]
    RecordNotFound {
        #[cfg(feature = "std")]
        record_name: String,
        #[cfg(not(feature = "std"))]
        _record_name: (),
    },

    /// Invalid operation attempted
    #[cfg_attr(feature = "std", error("Invalid operation '{operation}': {reason}"))]
    InvalidOperation {
        #[cfg(feature = "std")]
        operation: String,
        #[cfg(feature = "std")]
        reason: String,
        #[cfg(not(feature = "std"))]
        _operation: (),
        #[cfg(not(feature = "std"))]
        _reason: (),
    },

    // ===== Configuration Errors (0x4000-0x4FFF) =====
    /// Missing required configuration parameter
    #[cfg_attr(feature = "std", error("Missing configuration parameter: {parameter}"))]
    MissingConfiguration {
        #[cfg(feature = "std")]
        parameter: String,
        #[cfg(not(feature = "std"))]
        _parameter: (),
    },

    // ===== Runtime Errors (0x7002 & 0x5000-0x5FFF) =====
    /// Runtime execution error (task spawning, scheduling, etc.)
    #[cfg_attr(feature = "std", error("Runtime error: {message}"))]
    RuntimeError {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
    },

    /// Resource temporarily unavailable (used by adapters)
    #[cfg_attr(feature = "std", error("Resource unavailable: {resource_name}"))]
    ResourceUnavailable {
        resource_type: u8,
        #[cfg(feature = "std")]
        resource_name: String,
        #[cfg(not(feature = "std"))]
        _resource_name: (),
    },

    // ===== Hardware Errors (0x6000-0x6FFF) - Embedded Only =====
    /// Hardware-specific errors for embedded/MCU environments
    #[cfg_attr(
        feature = "std",
        error("Hardware error: component {component}, code 0x{error_code:04X}")
    )]
    HardwareError {
        component: u8,
        error_code: u16,
        #[cfg(feature = "std")]
        description: String,
        #[cfg(not(feature = "std"))]
        _description: (),
    },

    // ===== Internal Errors (0x7001) =====
    /// Internal error for unexpected conditions
    #[cfg_attr(feature = "std", error("Internal error (0x{code:04X}): {message}"))]
    Internal {
        code: u32,
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
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

// ===== no_std Display Implementation =====
#[cfg(not(feature = "std"))]
impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (code, message) = match self {
            DbError::ConnectionFailed { .. } => (0x1002, "Connection failed"),
            DbError::BufferFull { .. } => (0x2002, "Buffer full"),
            DbError::BufferLagged { .. } => (0xA001, "Buffer consumer lagged"),
            DbError::BufferClosed { .. } => (0xA002, "Buffer channel closed"),
            DbError::OutboxNotFound { .. } => (0xA003, "Outbox not found"),
            DbError::OutboxFull { .. } => (0xA004, "Outbox channel full"),
            DbError::OutboxClosed { .. } => (0xA005, "Outbox channel closed"),
            DbError::OutboxAlreadyExists { .. } => (0xA006, "Outbox already exists"),
            DbError::RecordNotFound { .. } => (0x7003, "Record not found"),
            DbError::InvalidOperation { .. } => (0x7004, "Invalid operation"),
            DbError::MissingConfiguration { .. } => (0x4002, "Missing configuration"),
            DbError::RuntimeError { .. } => (0x7002, "Runtime error"),
            DbError::ResourceUnavailable { .. } => (0x5002, "Resource unavailable"),
            DbError::HardwareError { .. } => (0x6001, "Hardware error"),
            DbError::Internal { .. } => (0x7001, "Internal error"),
        };
        write!(f, "Error 0x{:04X}: {}", code, message)
    }
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
            #[cfg(feature = "std")]
            description: String::new(),
            #[cfg(not(feature = "std"))]
            _description: (),
        }
    }

    /// Creates an internal error with a specific error code
    pub fn internal(code: u32) -> Self {
        DbError::Internal {
            code,
            #[cfg(feature = "std")]
            message: String::new(),
            #[cfg(not(feature = "std"))]
            _message: (),
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

    /// Returns a numeric error code for embedded environments
    pub const fn error_code(&self) -> u32 {
        match self {
            // Network errors: 0x1000-0x1FFF
            DbError::ConnectionFailed { .. } => 0x1002,

            // Capacity errors: 0x2000-0x2FFF
            DbError::BufferFull { .. } => 0x2002,

            // Configuration errors: 0x4000-0x4FFF
            DbError::MissingConfiguration { .. } => 0x4002,

            // Resource errors: 0x5000-0x5FFF
            DbError::ResourceUnavailable { .. } => 0x5002,

            // Hardware errors: 0x6000-0x6FFF
            DbError::HardwareError { .. } => 0x6001,

            // Internal errors: 0x7000-0x7FFF
            DbError::Internal { .. } => 0x7001,
            DbError::RuntimeError { .. } => 0x7002,
            DbError::RecordNotFound { .. } => 0x7003,
            DbError::InvalidOperation { .. } => 0x7004,

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

            // Buffer and Outbox operation errors: 0xA000-0xAFFF
            DbError::BufferLagged { .. } => 0xA001,
            DbError::BufferClosed { .. } => 0xA002,
            DbError::OutboxNotFound { .. } => 0xA003,
            DbError::OutboxFull { .. } => 0xA004,
            DbError::OutboxClosed { .. } => 0xA005,
            DbError::OutboxAlreadyExists { .. } => 0xA006,
        }
    }

    /// Returns the error category (high nibble)
    pub const fn error_category(&self) -> u32 {
        self.error_code() & 0xF000
    }

    /// Helper to prepend context to a message string
    #[cfg(feature = "std")]
    fn prepend_context<S: Into<String>>(existing: &mut String, new_context: S) {
        let new_context = new_context.into();
        existing.insert_str(0, ": ");
        existing.insert_str(0, &new_context);
    }

    /// Adds additional context to an error (std only)
    #[cfg(feature = "std")]
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
            DbError::OutboxNotFound { mut type_name } => {
                Self::prepend_context(&mut type_name, context);
                DbError::OutboxNotFound { type_name }
            }
            DbError::OutboxFull {
                capacity,
                mut type_name,
            } => {
                Self::prepend_context(&mut type_name, context);
                DbError::OutboxFull {
                    capacity,
                    type_name,
                }
            }
            DbError::OutboxClosed { mut type_name } => {
                Self::prepend_context(&mut type_name, context);
                DbError::OutboxClosed { type_name }
            }
            DbError::OutboxAlreadyExists { mut type_name } => {
                Self::prepend_context(&mut type_name, context);
                DbError::OutboxAlreadyExists { type_name }
            }
            DbError::RecordNotFound { mut record_name } => {
                Self::prepend_context(&mut record_name, context);
                DbError::RecordNotFound { record_name }
            }
            DbError::InvalidOperation {
                operation,
                mut reason,
            } => {
                Self::prepend_context(&mut reason, context);
                DbError::InvalidOperation { operation, reason }
            }
            DbError::MissingConfiguration { mut parameter } => {
                Self::prepend_context(&mut parameter, context);
                DbError::MissingConfiguration { parameter }
            }
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
            // Convert simple I/O and JSON errors to context variants
            DbError::Io { source } => DbError::IoWithContext {
                context: context.into(),
                source,
            },
            DbError::Json { source } => DbError::JsonWithContext {
                context: context.into(),
                source,
            },
            // Prepend to existing context for context variants
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_error_codes() {
        let connection_error = DbError::ConnectionFailed {
            #[cfg(feature = "std")]
            endpoint: "localhost".to_string(),
            #[cfg(feature = "std")]
            reason: "timeout".to_string(),
            #[cfg(not(feature = "std"))]
            _endpoint: (),
            #[cfg(not(feature = "std"))]
            _reason: (),
        };
        assert_eq!(connection_error.error_code(), 0x1002);
        assert_eq!(connection_error.error_category(), 0x1000);

        let buffer_error = DbError::BufferFull {
            size: 1024,
            #[cfg(feature = "std")]
            buffer_name: String::new(),
            #[cfg(not(feature = "std"))]
            _buffer_name: (),
        };
        assert_eq!(buffer_error.error_code(), 0x2002);
    }

    #[test]
    fn test_helper_methods() {
        let connection_error = DbError::ConnectionFailed {
            #[cfg(feature = "std")]
            endpoint: "localhost".to_string(),
            #[cfg(feature = "std")]
            reason: "timeout".to_string(),
            #[cfg(not(feature = "std"))]
            _endpoint: (),
            #[cfg(not(feature = "std"))]
            _reason: (),
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

    #[cfg(feature = "std")]
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
}
