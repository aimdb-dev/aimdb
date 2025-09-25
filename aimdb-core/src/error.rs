//! Error handling for AimDB core operations
//!
//! This module provides a unified error type system that works across all AimDB
//! target platforms: MCU (no_std), edge devices and cloud environments.
//!
//! # Platform Compatibility
//!
//! The error system is designed with conditional compilation to optimize for
//! different deployment targets:
//!
//! - **MCU/Embedded**: Minimal memory footprint (24 bytes) with `no_std` compatibility
//! - **Edge/Desktop**: Rich error context (56 bytes) with standard library features  
//! - **Cloud**: Full error chains and debugging capabilities with thiserror integration
//!
//! # Error Categories
//!
//! The [`DbError`] enum covers all operational scenarios:
//!
//! - **Network**: Connection timeouts, protocol errors, endpoint failures
//! - **Capacity**: Memory limits, buffer overflows, resource exhaustion  
//! - **Serialization**: Format errors, data corruption, encoding failures
//! - **Configuration**: Invalid settings, missing parameters, validation errors
//! - **Resource**: Allocation failures, unavailable resources, system limits
//! - **Hardware**: MCU peripheral errors, device initialization, hardware faults
//!
//! # Usage Examples
//!
//! ## Basic Error Handling
//!
//! ```rust
//! use aimdb_core::{DbError, DbResult};
//!
//! fn database_operation() -> DbResult<String> {
//!     // Simulate a capacity error
//!     Err(DbError::CapacityExceeded {
//!         current: 1024,
//!         limit: 1000,
//!         #[cfg(feature = "std")]
//!         resource_type: "memory".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _resource_type: (),
//!     })
//! }
//!
//! // Handle the result
//! match database_operation() {
//!     Ok(value) => println!("Success: {}", value),
//!     Err(DbError::CapacityExceeded { current, limit, .. }) => {
//!         println!("Over capacity: {}/{}", current, limit);
//!     }
//!     Err(other) => println!("Other error: {:?}", other),
//! }
//! ```
//!
//! ## Network Error Handling
//!
//! ```rust
//! # use aimdb_core::{DbError, DbResult};
//! fn connect_to_database() -> DbResult<()> {
//!     Err(DbError::NetworkTimeout {
//!         timeout_ms: 5000,
//!         #[cfg(feature = "std")]
//!         context: "Failed to reach database server".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _context: (),
//!     })
//! }
//! ```
//!
//! ## Hardware Error Handling (Embedded)
//!
//! ```rust
//! # use aimdb_core::{DbError, DbResult};
//! fn init_spi_peripheral() -> DbResult<()> {
//!     Err(DbError::PeripheralInitFailed {
//!         peripheral_id: 2, // SPI2
//!         #[cfg(feature = "std")]
//!         peripheral: "SPI2".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _peripheral: (),
//!     })
//! }
//! ```
//!
//! # Feature Flags
//!
//! - `std` (default): Enables rich error messages and thiserror integration
//! - `no_std`: Minimal error footprint for embedded targets
//!
//! # Memory Usage
//!
//! - **std mode**: 56 bytes per error instance
//! - **no_std mode**: 24 bytes per error instance
//! - Both modes stay well under the 64-byte embedded constraint
//!
//! # Migration Guide
//!
//! When upgrading from other error systems:
//!
//! 1. Replace existing error types with [`DbError`] variants
//! 2. Use [`DbResult<T>`] instead of `Result<T, YourError>`
//! 3. Enable appropriate feature flags for your target platform
//! 4. Update error matching to use the new error categories

#[cfg(feature = "std")]
use thiserror::Error;

/// Unified error type for all AimDB operations across platforms
///
/// This enum covers all error scenarios that can occur during AimDB operations,
/// with conditional compilation to optimize for different target environments.
/// The design ensures memory efficiency for embedded targets while providing
/// rich error context in standard environments.
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum DbError {
    /// Network-related errors (timeouts, connection failures, protocol errors)
    #[cfg_attr(
        feature = "std",
        error("Network timeout after {timeout_ms}ms: {context}")
    )]
    NetworkTimeout {
        timeout_ms: u64,
        #[cfg(feature = "std")]
        context: String,
        #[cfg(not(feature = "std"))]
        _context: (),
    },

    /// Network connection establishment failures
    #[cfg_attr(feature = "std", error("Connection failed to {endpoint}: {reason}"))]
    ConnectionFailed {
        #[cfg(feature = "std")]
        endpoint: String,
        #[cfg(feature = "std")]
        reason: String,
        #[cfg(not(feature = "std"))]
        _endpoint: (),
    },

    /// Protocol-level network errors
    #[cfg_attr(feature = "std", error("Protocol error: {message}"))]
    ProtocolError {
        error_code: u32,
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
    },

    /// Storage capacity exceeded (memory, disk, or buffer limits)
    #[cfg_attr(
        feature = "std",
        error("Capacity exceeded: {current}/{limit} {resource_type}")
    )]
    CapacityExceeded {
        current: u64,
        limit: u64,
        #[cfg(feature = "std")]
        resource_type: String,
        #[cfg(not(feature = "std"))]
        _resource_type: (),
    },

    /// Buffer or queue is full
    #[cfg_attr(feature = "std", error("Buffer full: {buffer_name} ({size} items)"))]
    BufferFull {
        size: u32,
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },

    /// Data serialization/deserialization errors
    #[cfg_attr(feature = "std", error("Serialization failed: {details}"))]
    SerializationFailed {
        format: u8, // 0=JSON, 1=MessagePack, 2=CBOR, etc.
        #[cfg(feature = "std")]
        details: String,
        #[cfg(not(feature = "std"))]
        _details: (),
    },

    /// Invalid data format or corrupted data
    #[cfg_attr(feature = "std", error("Invalid data format: {description}"))]
    InvalidDataFormat {
        expected_format: u8,
        received_format: u8,
        #[cfg(feature = "std")]
        description: String,
        #[cfg(not(feature = "std"))]
        _description: (),
    },

    /// Configuration errors (invalid settings, missing parameters)
    #[cfg_attr(feature = "std", error("Invalid configuration: {parameter} = {value}"))]
    InvalidConfiguration {
        #[cfg(feature = "std")]
        parameter: String,
        #[cfg(feature = "std")]
        value: String,
        #[cfg(not(feature = "std"))]
        _parameter: (),
    },

    /// Missing required configuration
    #[cfg_attr(feature = "std", error("Missing configuration parameter: {parameter}"))]
    MissingConfiguration {
        #[cfg(feature = "std")]
        parameter: String,
        #[cfg(not(feature = "std"))]
        _parameter: (),
    },

    /// Resource allocation failures (memory, file handles, etc.)
    #[cfg_attr(feature = "std", error("Resource allocation failed: {details}"))]
    ResourceAllocationFailed {
        resource_type: u8, // 0=Memory, 1=FileHandle, 2=Socket, etc.
        requested_size: u32,
        #[cfg(feature = "std")]
        details: String,
        #[cfg(not(feature = "std"))]
        _details: (),
    },

    /// Resource temporarily unavailable
    #[cfg_attr(feature = "std", error("Resource unavailable: {resource_name}"))]
    ResourceUnavailable {
        resource_type: u8,
        #[cfg(feature = "std")]
        resource_name: String,
        #[cfg(not(feature = "std"))]
        _resource_name: (),
    },

    /// Hardware-specific errors (embedded/MCU environments)
    #[cfg_attr(feature = "std", error("Hardware error: {component} - {description}"))]
    HardwareError {
        component: u8, // 0=Timer, 1=GPIO, 2=SPI, 3=I2C, 4=UART, etc.
        error_code: u16,
        #[cfg(feature = "std")]
        description: String,
        #[cfg(not(feature = "std"))]
        _description: (),
    },

    /// Hardware peripheral not available or failed to initialize
    #[cfg_attr(
        feature = "std",
        error("Peripheral initialization failed: {peripheral}")
    )]
    PeripheralInitFailed {
        peripheral_id: u8,
        #[cfg(feature = "std")]
        peripheral: String,
        #[cfg(not(feature = "std"))]
        _peripheral: (),
    },

    /// Generic internal errors for unexpected conditions
    #[cfg_attr(feature = "std", error("Internal error: {message}"))]
    Internal {
        code: u32,
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
    },
}

#[cfg(not(feature = "std"))]
impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            DbError::NetworkTimeout { .. } => "Network timeout",
            DbError::ConnectionFailed { .. } => "Connection failed",
            DbError::ProtocolError { .. } => "Protocol error",
            DbError::CapacityExceeded { .. } => "Capacity exceeded",
            DbError::BufferFull { .. } => "Buffer full",
            DbError::SerializationFailed { .. } => "Serialization failed",
            DbError::InvalidDataFormat { .. } => "Invalid data format",
            DbError::InvalidConfiguration { .. } => "Invalid configuration",
            DbError::MissingConfiguration { .. } => "Missing configuration parameter",
            DbError::ResourceAllocationFailed { .. } => "Resource allocation failed",
            DbError::ResourceUnavailable { .. } => "Resource unavailable",
            DbError::HardwareError { .. } => "Hardware error",
            DbError::PeripheralInitFailed { .. } => "Peripheral initialization failed",
            DbError::Internal { .. } => "Internal error",
        };
        write!(f, "{}", message)
    }
}

impl DbError {
    /// Creates a network timeout error with the specified timeout duration
    pub fn network_timeout(timeout_ms: u64) -> Self {
        DbError::NetworkTimeout {
            timeout_ms,
            #[cfg(feature = "std")]
            context: String::new(),
            #[cfg(not(feature = "std"))]
            _context: (),
        }
    }

    /// Creates a network timeout error with context (std only)
    #[cfg(feature = "std")]
    pub fn network_timeout_with_context(timeout_ms: u64, context: impl Into<String>) -> Self {
        DbError::NetworkTimeout {
            timeout_ms,
            context: context.into(),
        }
    }

    /// Creates a capacity exceeded error
    pub fn capacity_exceeded(current: u64, limit: u64) -> Self {
        DbError::CapacityExceeded {
            current,
            limit,
            #[cfg(feature = "std")]
            resource_type: String::new(),
            #[cfg(not(feature = "std"))]
            _resource_type: (),
        }
    }

    /// Creates a capacity exceeded error with resource type (std only)
    #[cfg(feature = "std")]
    pub fn capacity_exceeded_with_type(
        current: u64,
        limit: u64,
        resource_type: impl Into<String>,
    ) -> Self {
        DbError::CapacityExceeded {
            current,
            limit,
            resource_type: resource_type.into(),
        }
    }

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
        matches!(
            self,
            DbError::NetworkTimeout { .. }
                | DbError::ConnectionFailed { .. }
                | DbError::ProtocolError { .. }
        )
    }

    /// Returns true if this is a capacity-related error  
    pub fn is_capacity_error(&self) -> bool {
        matches!(
            self,
            DbError::CapacityExceeded { .. } | DbError::BufferFull { .. }
        )
    }

    /// Returns true if this is a hardware-related error
    pub fn is_hardware_error(&self) -> bool {
        matches!(
            self,
            DbError::HardwareError { .. } | DbError::PeripheralInitFailed { .. }
        )
    }
}

/// Type alias for Results using DbError
///
/// This is the standard Result type used throughout AimDB for operations
/// that may fail. It provides a consistent error handling interface across
/// all components and platforms.
pub type DbResult<T> = Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_size_constraint() {
        // Ensure DbError size is ≤64 bytes on embedded targets
        let size = core::mem::size_of::<DbError>();
        assert!(
            size <= 64,
            "DbError size ({} bytes) exceeds 64-byte limit for embedded targets",
            size
        );

        // Print size for monitoring
        println!("DbError size: {} bytes", size);
    }

    #[test]
    fn test_error_creation() {
        // Test creating various error types
        let timeout_error = DbError::NetworkTimeout {
            timeout_ms: 5000,
            #[cfg(feature = "std")]
            context: "Connection to database".to_string(),
            #[cfg(not(feature = "std"))]
            _context: (),
        };

        let capacity_error = DbError::CapacityExceeded {
            current: 1024,
            limit: 1000,
            #[cfg(feature = "std")]
            resource_type: "memory".to_string(),
            #[cfg(not(feature = "std"))]
            _resource_type: (),
        };

        // Test that errors can be formatted
        let timeout_msg = format!("{:?}", timeout_error);
        let capacity_msg = format!("{:?}", capacity_error);

        assert!(timeout_msg.contains("NetworkTimeout"));
        assert!(capacity_msg.contains("CapacityExceeded"));
    }

    #[test]
    fn test_dbresult_usage() {
        // Test DbResult type alias usage
        fn example_operation() -> DbResult<String> {
            Ok("success".to_string())
        }

        fn failing_operation() -> DbResult<String> {
            Err(DbError::Internal {
                code: 500,
                #[cfg(feature = "std")]
                message: "Test error".to_string(),
                #[cfg(not(feature = "std"))]
                _message: (),
            })
        }

        assert!(example_operation().is_ok());
        assert!(failing_operation().is_err());
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn test_no_std_display() {
        use core::fmt::Write;

        let error = DbError::NetworkTimeout {
            timeout_ms: 1000,
            _context: (),
        };

        let mut buffer = heapless::String::<64>::new();
        write!(&mut buffer, "{}", error).unwrap();
        assert_eq!(buffer.as_str(), "Network timeout");
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_error_traits() {
        let error = DbError::NetworkTimeout {
            timeout_ms: 1000,
            context: "Test connection".to_string(),
        };

        // Test that error implements std::error::Error
        let _: &dyn std::error::Error = &error;

        // Test Display implementation
        let display_msg = format!("{}", error);
        assert!(display_msg.contains("Network timeout"));
        assert!(display_msg.contains("1000ms"));
        assert!(display_msg.contains("Test connection"));
    }

    #[test]
    fn test_helper_methods() {
        // Test helper constructors
        let timeout_error = DbError::network_timeout(5000);
        assert!(matches!(
            timeout_error,
            DbError::NetworkTimeout {
                timeout_ms: 5000,
                ..
            }
        ));

        let capacity_error = DbError::capacity_exceeded(1024, 512);
        assert!(matches!(
            capacity_error,
            DbError::CapacityExceeded {
                current: 1024,
                limit: 512,
                ..
            }
        ));

        let hardware_error = DbError::hardware_error(2, 404);
        assert!(matches!(
            hardware_error,
            DbError::HardwareError {
                component: 2,
                error_code: 404,
                ..
            }
        ));

        let internal_error = DbError::internal(500);
        assert!(matches!(
            internal_error,
            DbError::Internal { code: 500, .. }
        ));

        // Test error category methods
        assert!(timeout_error.is_network_error());
        assert!(!timeout_error.is_capacity_error());
        assert!(!timeout_error.is_hardware_error());

        assert!(!capacity_error.is_network_error());
        assert!(capacity_error.is_capacity_error());
        assert!(!capacity_error.is_hardware_error());

        assert!(!hardware_error.is_network_error());
        assert!(!hardware_error.is_capacity_error());
        assert!(hardware_error.is_hardware_error());
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_helper_methods() {
        let timeout_error = DbError::network_timeout_with_context(3000, "Database connection");
        if let DbError::NetworkTimeout {
            timeout_ms,
            context,
        } = timeout_error
        {
            assert_eq!(timeout_ms, 3000);
            assert_eq!(context, "Database connection");
        } else {
            panic!("Expected NetworkTimeout error");
        }

        let capacity_error = DbError::capacity_exceeded_with_type(2048, 1024, "memory");
        if let DbError::CapacityExceeded {
            current,
            limit,
            resource_type,
        } = capacity_error
        {
            assert_eq!(current, 2048);
            assert_eq!(limit, 1024);
            assert_eq!(resource_type, "memory");
        } else {
            panic!("Expected CapacityExceeded error");
        }
    }
}
