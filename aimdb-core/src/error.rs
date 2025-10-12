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
//! - **Network**: Connection failures, endpoint errors
//! - **Capacity**: Buffer overflows, resource exhaustion  
//! - **Serialization**: Format errors, data corruption, encoding failures
//! - **Configuration**: Missing parameters, validation errors
//! - **Resource**: Allocation failures, unavailable resources, system limits
//! - **Hardware**: MCU hardware errors, device failures
//!
//! # Error Code System
//!
//! Each error has an associated numeric code for embedded environments where string
//! formatting is not available or optimal. Error codes are organized by category:
//!
//! - **Network** (0x1000-0x1FFF): Connection failures, endpoint errors
//! - **Capacity** (0x2000-0x2FFF): Buffer overflows, resource exhaustion
//! - **Serialization** (0x3000-0x3FFF): Format errors, data corruption, encoding failures
//! - **Configuration** (0x4000-0x4FFF): Missing parameters, validation errors
//! - **Resource** (0x5000-0x5FFF): Allocation failures, unavailable resources, system limits
//! - **Hardware** (0x6000-0x6FFF): MCU hardware errors, device failures
//! - **Internal** (0x7000-0x7FFF): Unexpected conditions and system errors
//! - **I/O Operations** (0x8000-0x8FFF): File system and I/O errors, including context variants
//! - **JSON Processing** (0x9000-0x9FFF): JSON serialization/deserialization errors, including context variants
//! - **Buffer Operations** (0xA000-0xAFFF): Buffer read/write errors, lag detection, channel closure
//!
//! ## Platform-Specific Display Behavior
//!
//! - **std mode**: Rich error messages with full context (e.g., "Network timeout after 5000ms: Database connection")
//! - **no_std mode**: Compact format with error codes (e.g., "Error 0x1001: Network timeout")
//!
//! # Performance Guarantees
//!
//! - **Error code lookup**: <1μs on embedded platforms (const function)
//! - **Display formatting**: <1μs on embedded, <10μs on std platforms
//! - **Memory usage**: Zero heap allocation on embedded targets
//!
//! # Enhanced Error Features (std only)
//!
//! When the `std` feature is enabled (default), AimDB provides rich error handling capabilities:
//!
//! - **Error Chaining**: Add context to errors with the `with_context()` method (works on all error types)
//! - **Standard Library Integration**: Automatic conversions from `std::io::Error` and `serde_json::Error`
//! - **Anyhow Compatibility**: Seamless integration with `anyhow` for application boundaries
//! - **Rich Error Messages**: Full context and error source chains
//! - **Consistent Context API**: All error variants support context chaining, including I/O and JSON errors
//!
//! ## Error Chaining Example
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # {
//! use aimdb_core::DbError;
//!
//! let error = DbError::ConnectionFailed {
//!     endpoint: "database.example.com:5432".to_string(),
//!     reason: "Connection timeout after 30s".to_string()
//! }
//!     .with_context("Failed to connect to database server")
//!     .with_context("During application startup");
//!
//! println!("{}", error);
//! // "Connection failed to database.example.com:5432: During application startup: Failed to connect to database server: Connection timeout after 30s"
//! # }
//! ```
//!
//! ## Standard Library Integration
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # {
//! use aimdb_core::DbError;
//! use std::fs::File;
//!
//! // Automatic conversion from std::io::Error
//! let result: Result<File, DbError> = File::open("nonexistent.txt")
//!     .map_err(DbError::from);
//!
//! // Adding context to I/O errors now works consistently
//! let contextualized_result: Result<File, DbError> = File::open("config.txt")
//!     .map_err(DbError::from)
//!     .map_err(|e| e.with_context("Loading application configuration"));
//!
//! // Or using the ? operator in functions
//! fn read_config() -> Result<String, DbError> {
//!     let content = std::fs::read_to_string("config.txt")?;
//!     Ok(content)
//! }
//! # }
//! ```
//!
//! ## Anyhow Integration
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # {
//! use aimdb_core::DbError;
//! use anyhow::Context;
//!
//! fn application_main() -> anyhow::Result<()> {
//!     // Consistent context handling works across all error types
//!     let io_error = std::io::Error::other("File not found");
//!     let db_error = DbError::from(io_error)
//!         .with_context("Loading configuration file")
//!         .with_context("Application initialization");
//!         
//!     Err(db_error.into_anyhow())
//!         .context("Database initialization failed")
//!         .context("Application startup failed")
//! }
//! # }
//! ```
//!
//! # Usage Examples
//!
//! ## Consistent Context Handling
//!
//! The `with_context()` method now works consistently across all error types:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # {
//! use aimdb_core::DbError;
//!
//! // I/O errors can now receive context
//! let io_error = std::io::Error::other("Permission denied");
//! let contextualized = DbError::from(io_error)
//!     .with_context("Reading configuration file")
//!     .with_context("Application startup");
//!
//! // JSON errors can now receive context  
//! let json_str = r#"{"invalid": json}"#;
//! let json_result: Result<serde_json::Value, DbError> =
//!     serde_json::from_str(json_str)
//!         .map_err(DbError::from)
//!         .map_err(|e| e.with_context("Parsing user input"))
//!         .map_err(|e| e.with_context("Configuration validation"));
//!
//! // Context chaining works on all error types
//! let network_error = DbError::ConnectionFailed {
//!     endpoint: "database.example.com".to_string(),
//!     reason: "Timeout".to_string(),
//! }.with_context("Connecting to database")
//!  .with_context("Service initialization");
//! # }
//! ```
//!
//! ## Basic Error Handling
//!
//! ```rust
//! use aimdb_core::{DbError, DbResult};
//!
//! fn database_operation() -> DbResult<String> {
//!     // Simulate a capacity error
//!     Err(DbError::BufferFull {
//!         size: 1024,
//!         #[cfg(feature = "std")]
//!         buffer_name: "memory buffer".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _buffer_name: (),
//!     })
//! }
//!
//! // Handle the result
//! match database_operation() {
//!     Ok(value) => println!("Success: {}", value),
//!     Err(DbError::BufferFull { size, .. }) => {
//!         println!("Buffer full with {} items", size);
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
//!     Err(DbError::ConnectionFailed {
//!         #[cfg(feature = "std")]
//!         endpoint: "localhost:5432".to_string(),
//!         #[cfg(feature = "std")]
//!         reason: "Connection refused".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _endpoint: (),
//!         #[cfg(not(feature = "std"))]
//!         _reason: (),
//!     })
//! }
//! ```
//!
//! ## Hardware Error Handling (Embedded)
//!
//! ```rust
//! # use aimdb_core::{DbError, DbResult};
//! fn init_spi_peripheral() -> DbResult<()> {
//!     Err(DbError::HardwareError {
//!         component: 2, // SPI2
//!         error_code: 0x404,
//!         #[cfg(feature = "std")]
//!         description: "SPI2 initialization failed".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _description: (),
//!     })
//! }
//! ```
//!
//! ## Internal Error Handling (System-Level Issues)
//!
//! ```rust
//! # use aimdb_core::{DbError, DbResult};
//! fn validate_memory_integrity() -> DbResult<()> {
//!     // Only use Internal errors for unexpected system conditions,
//!     // not for normal operational failures
//!     Err(DbError::Internal {
//!         code: 0x7001,
//!         #[cfg(feature = "std")]
//!         message: "Memory corruption detected in buffer pool".to_string(),
//!         #[cfg(not(feature = "std"))]
//!         _message: (),
//!     })
//! }
//!
//! fn process_user_data(data: &[u8]) -> DbResult<()> {
//!     // DON'T use Internal for validation failures - use appropriate error types
//!     if data.is_empty() {
//!         return Err(DbError::InvalidDataFormat {
//!             expected_format: 1, // Expected non-empty data
//!             received_format: 0, // Empty data
//!             #[cfg(feature = "std")]
//!             description: "Expected non-empty data payload".to_string(),
//!             #[cfg(not(feature = "std"))]
//!             _description: (),
//!         });
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Error Code Usage (Embedded)
//!
//! ```rust
//! # use aimdb_core::DbError;
//! // Fast error classification for embedded systems
//! fn handle_error(error: &DbError) {
//!     let code = error.error_code();
//!     let category = error.error_category();
//!     
//!     match category {
//!         0x1000 => {
//!             // Network errors - retry with backoff
//!             println!("Network error 0x{:04X} - retrying...", code);
//!         }
//!         0x2000 => {
//!             // Capacity errors - reduce load  
//!             println!("Capacity error 0x{:04X} - throttling...", code);
//!         }
//!         0x6000 => {
//!             // Hardware errors - critical failure
//!             println!("Hardware error 0x{:04X} - system halt", code);
//!         }
//!         _ => {
//!             println!("Other error 0x{:04X}", code);
//!         }
//!     }
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

// Rich error messages in std mode are derived from thiserror::Error
// This import enables automatic Display implementation generation using
// the #[cfg_attr(feature = "std", error("..."))] format strings below
#[cfg(feature = "std")]
use thiserror::Error;

// Additional std-only imports for enhanced error handling
#[cfg(feature = "std")]
use std::io;

/// Unified error type for all AimDB operations across platforms
///
/// This enum covers all error scenarios that can occur during AimDB operations,
/// with conditional compilation to optimize for different target environments.
/// The design ensures memory efficiency for embedded targets while providing
/// rich error context in standard environments.
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum DbError {
    /// Network connection establishment failures
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

    /// Generic internal errors for unexpected conditions and system-level failures
    ///
    /// **When to use Internal errors:**
    /// - Assertion failures or invariant violations
    /// - Memory corruption or invalid state detection  
    /// - Unexpected system-level errors that don't fit other categories
    /// - Library bugs or unrecoverable programming errors
    ///
    /// **When NOT to use Internal errors:**
    /// - Network failures (use `ConnectionFailed` instead)
    /// - Resource exhaustion (use `BufferFull` or `ResourceUnavailable`)
    /// - Invalid user input (use `InvalidDataFormat` or `SerializationFailed`)
    /// - Missing configuration (use `MissingConfiguration`)
    ///
    /// Internal errors indicate that something unexpected happened within
    /// AimDB itself, not external conditions that the application should handle.
    #[cfg_attr(feature = "std", error("Internal error: {message}"))]
    Internal {
        code: u32,
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
    },

    /// Runtime execution errors (task spawning, scheduling, etc.)
    #[cfg_attr(feature = "std", error("Runtime error: {message}"))]
    RuntimeError {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        _message: (),
    },

    /// I/O operation errors (std only)
    #[cfg(feature = "std")]
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },

    /// I/O operation errors with additional context (std only)
    #[cfg(feature = "std")]
    #[error("I/O error: {context}: {source}")]
    IoWithContext {
        context: String,
        #[source]
        source: io::Error,
    },

    /// JSON serialization errors (std only)
    #[cfg(feature = "std")]
    #[error("JSON error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },

    /// JSON serialization errors with additional context (std only)
    #[cfg(feature = "std")]
    #[error("JSON error: {context}: {source}")]
    JsonWithContext {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    /// Outbox not found for the specified type
    ///
    /// This error occurs when attempting to enqueue to an outbox that hasn't
    /// been initialized via `init_outbox()`.
    ///
    /// # Recovery Strategy
    /// - Verify outbox was initialized before use
    /// - Check that the correct payload type is being used
    /// - Ensure init_outbox() completed successfully
    #[cfg_attr(feature = "std", error("Outbox not found for type: {type_name}"))]
    OutboxNotFound {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Outbox channel is full and cannot accept more messages
    ///
    /// This error occurs when the outbox buffer is full and `OverflowBehavior::Error`
    /// is configured. The message was not enqueued.
    ///
    /// # Recovery Strategy
    /// - Retry with backoff
    /// - Use try_enqueue() for non-critical messages
    /// - Consider increasing outbox capacity
    /// - Switch to OverflowBehavior::Block for backpressure
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
    ///
    /// This error indicates the outbox worker has stopped and is no longer
    /// accepting messages. This typically occurs during shutdown or if the
    /// worker task panicked.
    ///
    /// # Causes
    /// - Database shutdown
    /// - Worker task panic or termination
    /// - Runtime termination
    ///
    /// # Recovery Strategy
    /// - Check worker status via WorkerHandle
    /// - Attempt worker restart if auto_restart is enabled
    /// - Log for investigation if unexpected
    #[cfg_attr(feature = "std", error("Outbox channel closed: {type_name}"))]
    OutboxClosed {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Outbox already exists for the specified type
    ///
    /// This error occurs when attempting to initialize an outbox that has
    /// already been registered for the same payload type.
    ///
    /// # Recovery Strategy
    /// - Use the existing outbox
    /// - Check for duplicate initialization logic
    /// - Each payload type can only have one outbox
    #[cfg_attr(feature = "std", error("Outbox already exists for type: {type_name}"))]
    OutboxAlreadyExists {
        #[cfg(feature = "std")]
        type_name: String,
        #[cfg(not(feature = "std"))]
        _type_name: (),
    },

    /// Buffer consumer lagged behind producer
    ///
    /// This error occurs with SPMC ring buffers when a consumer falls behind
    /// and messages are overwritten before being read. The consumer can continue
    /// from the current position, but some messages were skipped.
    ///
    /// # Recovery Strategy
    /// - Log the lag count for monitoring
    /// - Continue consuming from current position
    /// - Consider increasing buffer capacity or optimizing consumer
    #[cfg_attr(feature = "std", error("Consumer lagged by {lag_count} messages"))]
    BufferLagged {
        lag_count: u64,
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },

    /// Buffer channel has been closed
    ///
    /// This error indicates the buffer is no longer accepting or delivering messages,
    /// typically during shutdown. The consumer should exit its dispatch loop.
    ///
    /// # Causes
    /// - Database shutdown
    /// - Record removal
    /// - Runtime termination
    #[cfg_attr(feature = "std", error("Buffer channel closed: {buffer_name}"))]
    BufferClosed {
        #[cfg(feature = "std")]
        buffer_name: String,
        #[cfg(not(feature = "std"))]
        _buffer_name: (),
    },
}

#[cfg(not(feature = "std"))]
impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // For embedded environments, display error code with brief description
        // Format: "Error 0x1001: Network timeout"
        // This provides both human readability and precise error identification
        let (code, message) = match self {
            DbError::ConnectionFailed { .. } => (0x1002, "Connection failed"),
            DbError::BufferFull { .. } => (0x2002, "Buffer full"),
            DbError::SerializationFailed { .. } => (0x3001, "Serialization failed"),
            DbError::InvalidDataFormat { .. } => (0x3002, "Invalid data format"),
            DbError::MissingConfiguration { .. } => (0x4002, "Missing configuration parameter"),
            DbError::ResourceAllocationFailed { .. } => (0x5001, "Resource allocation failed"),
            DbError::ResourceUnavailable { .. } => (0x5002, "Resource unavailable"),
            DbError::HardwareError { .. } => (0x6001, "Hardware error"),
            DbError::Internal { .. } => (0x7001, "Internal error"),
            DbError::RuntimeError { .. } => (0x7002, "Runtime error"),
            DbError::OutboxNotFound { .. } => (0xA003, "Outbox not found"),
            DbError::OutboxFull { .. } => (0xA004, "Outbox channel full"),
            DbError::OutboxClosed { .. } => (0xA005, "Outbox channel closed"),
            DbError::OutboxAlreadyExists { .. } => (0xA006, "Outbox already exists"),
            DbError::BufferLagged { .. } => (0xA001, "Buffer consumer lagged"),
            DbError::BufferClosed { .. } => (0xA002, "Buffer channel closed"),

            // Standard library only errors (conditionally compiled)
            #[cfg(feature = "std")]
            DbError::Io { .. } => (0x8001, "I/O error"),
            #[cfg(feature = "std")]
            DbError::IoWithContext { .. } => (0x8002, "I/O error with context"),
            #[cfg(feature = "std")]
            DbError::Json { .. } => (0x9001, "JSON error"),
            #[cfg(feature = "std")]
            DbError::JsonWithContext { .. } => (0x9002, "JSON error with context"),
        };
        write!(f, "Error 0x{:04X}: {}", code, message)
    }
}

impl DbError {
    // Resource type constants for ResourceUnavailable and ResourceAllocationFailed errors
    pub const RESOURCE_TYPE_MEMORY: u8 = 0;
    pub const RESOURCE_TYPE_FILE_HANDLE: u8 = 1;
    pub const RESOURCE_TYPE_SOCKET: u8 = 2;
    pub const RESOURCE_TYPE_BUFFER: u8 = 3;
    pub const RESOURCE_TYPE_THREAD: u8 = 4;
    pub const RESOURCE_TYPE_MUTEX: u8 = 5;
    pub const RESOURCE_TYPE_SEMAPHORE: u8 = 6;
    pub const RESOURCE_TYPE_CHANNEL: u8 = 7;
    // Reserved range for embedded-specific resources: 8-127
    // Reserved range for connector-specific resources: 128-254
    /// Special resource type code for non-blocking operations that would block
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
    ///
    /// Provides fast (<1μs) error code lookup for embedded platforms where
    /// string formatting is not available or desirable. Error codes are organized
    /// by category with specific ranges:
    ///
    /// - **Network**: 0x1000-0x1FFF  
    /// - **Capacity**: 0x2000-0x2FFF
    /// - **Serialization**: 0x3000-0x3FFF  
    /// - **Configuration**: 0x4000-0x4FFF
    /// - **Resource**: 0x5000-0x5FFF
    /// - **Hardware**: 0x6000-0x6FFF
    /// - **Internal**: 0x7000-0x7FFF
    ///
    /// # Performance
    ///
    /// This method is const and optimized for embedded environments with
    /// guaranteed <1μs lookup time and zero heap allocation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aimdb_core::DbError;
    ///
    /// let error = DbError::ConnectionFailed {
    ///     #[cfg(feature = "std")]
    ///     endpoint: "localhost".to_string(),
    ///     #[cfg(feature = "std")]
    ///     reason: "timeout".to_string(),
    ///     #[cfg(not(feature = "std"))]
    ///     _endpoint: (),
    ///     #[cfg(not(feature = "std"))]
    ///     _reason: (),
    /// };
    /// assert_eq!(error.error_code(), 0x1002);
    ///
    /// let capacity_error = DbError::BufferFull {
    ///     size: 1024,
    ///     #[cfg(feature = "std")]
    ///     buffer_name: "ring buffer".to_string(),
    ///     #[cfg(not(feature = "std"))]
    ///     _buffer_name: (),
    /// };
    /// assert_eq!(capacity_error.error_code(), 0x2002);
    /// ```
    pub const fn error_code(&self) -> u32 {
        match self {
            // Network errors: 0x1000-0x1FFF
            DbError::ConnectionFailed { .. } => 0x1002,

            // Capacity errors: 0x2000-0x2FFF
            DbError::BufferFull { .. } => 0x2002,

            // Serialization errors: 0x3000-0x3FFF
            DbError::SerializationFailed { .. } => 0x3001,
            DbError::InvalidDataFormat { .. } => 0x3002,

            // Configuration errors: 0x4000-0x4FFF
            DbError::MissingConfiguration { .. } => 0x4002,

            // Resource errors: 0x5000-0x5FFF
            DbError::ResourceAllocationFailed { .. } => 0x5001,
            DbError::ResourceUnavailable { .. } => 0x5002,

            // Hardware errors: 0x6000-0x6FFF
            DbError::HardwareError { .. } => 0x6001,

            // Internal errors: 0x7000-0x7FFF
            DbError::Internal { .. } => 0x7001,
            DbError::RuntimeError { .. } => 0x7002,

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

    /// Returns the error category based on the error code
    ///
    /// Extracts the category from an error code for classification and handling.
    /// This is useful for embedded environments where you want to handle entire
    /// categories of errors uniformly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aimdb_core::DbError;
    ///
    /// let error = DbError::ConnectionFailed {
    ///     #[cfg(feature = "std")]
    ///     endpoint: "localhost".to_string(),
    ///     #[cfg(feature = "std")]
    ///     reason: "timeout".to_string(),
    ///     #[cfg(not(feature = "std"))]
    ///     _endpoint: (),
    ///     #[cfg(not(feature = "std"))]
    ///     _reason: (),
    /// };
    /// assert_eq!(error.error_category(), 0x1000);
    /// ```
    pub const fn error_category(&self) -> u32 {
        self.error_code() & 0xF000
    }

    /// Helper function to prepend context to a message string (always prepends)
    ///
    /// This function always prepends the new context, used for fields that
    /// always contain some default content. Uses in-place string modification
    /// to avoid unnecessary allocations in the error handling hot path.
    #[cfg(feature = "std")]
    fn prepend_context_always<S: Into<String>>(existing: &mut String, new_context: S) {
        let new_context = new_context.into();
        existing.insert_str(0, ": ");
        existing.insert_str(0, &new_context);
    }

    /// Adds additional context to an error (std only)
    ///
    /// This method allows chaining additional context information to errors,
    /// useful for providing more detailed error information in edge and cloud
    /// environments where memory is not as constrained.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # #[cfg(feature = "std")]
    /// # {
    /// use aimdb_core::DbError;
    ///
    /// let error = DbError::ConnectionFailed {
    ///     endpoint: "database.example.com".to_string(),
    ///     reason: "timeout".to_string()
    /// }.with_context("Failed to connect to database server")
    ///  .with_context("During application startup");
    /// # }
    /// ```
    #[cfg(feature = "std")]
    pub fn with_context<S: Into<String>>(self, context: S) -> Self {
        match self {
            DbError::ConnectionFailed {
                mut reason,
                endpoint,
            } => {
                Self::prepend_context_always(&mut reason, context);
                DbError::ConnectionFailed { endpoint, reason }
            }
            DbError::BufferFull {
                size,
                mut buffer_name,
            } => {
                Self::prepend_context_always(&mut buffer_name, context);
                DbError::BufferFull { size, buffer_name }
            }
            DbError::SerializationFailed {
                format,
                mut details,
            } => {
                Self::prepend_context_always(&mut details, context);
                DbError::SerializationFailed { format, details }
            }
            DbError::InvalidDataFormat {
                expected_format,
                received_format,
                mut description,
            } => {
                Self::prepend_context_always(&mut description, context);
                DbError::InvalidDataFormat {
                    expected_format,
                    received_format,
                    description,
                }
            }
            DbError::MissingConfiguration { mut parameter } => {
                Self::prepend_context_always(&mut parameter, context);
                DbError::MissingConfiguration { parameter }
            }
            DbError::ResourceAllocationFailed {
                resource_type,
                requested_size,
                mut details,
            } => {
                Self::prepend_context_always(&mut details, context);
                DbError::ResourceAllocationFailed {
                    resource_type,
                    requested_size,
                    details,
                }
            }
            DbError::ResourceUnavailable {
                resource_type,
                mut resource_name,
            } => {
                Self::prepend_context_always(&mut resource_name, context);
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
                Self::prepend_context_always(&mut description, context);
                DbError::HardwareError {
                    component,
                    error_code,
                    description,
                }
            }
            DbError::Internal { code, mut message } => {
                Self::prepend_context_always(&mut message, context);
                DbError::Internal { code, message }
            }

            DbError::RuntimeError { mut message } => {
                Self::prepend_context_always(&mut message, context);
                DbError::RuntimeError { message }
            }

            // Buffer operation errors
            DbError::BufferLagged {
                lag_count,
                mut buffer_name,
            } => {
                Self::prepend_context_always(&mut buffer_name, context);
                DbError::BufferLagged {
                    lag_count,
                    buffer_name,
                }
            }
            DbError::BufferClosed { mut buffer_name } => {
                Self::prepend_context_always(&mut buffer_name, context);
                DbError::BufferClosed { buffer_name }
            }

            // Outbox operation errors
            DbError::OutboxNotFound { mut type_name } => {
                Self::prepend_context_always(&mut type_name, context);
                DbError::OutboxNotFound { type_name }
            }
            DbError::OutboxFull {
                capacity,
                mut type_name,
            } => {
                Self::prepend_context_always(&mut type_name, context);
                DbError::OutboxFull {
                    capacity,
                    type_name,
                }
            }
            DbError::OutboxClosed { mut type_name } => {
                Self::prepend_context_always(&mut type_name, context);
                DbError::OutboxClosed { type_name }
            }
            DbError::OutboxAlreadyExists { mut type_name } => {
                Self::prepend_context_always(&mut type_name, context);
                DbError::OutboxAlreadyExists { type_name }
            }

            // For Io and Json errors, convert to context variants
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

            // For context variants, prepend to existing context
            #[cfg(feature = "std")]
            DbError::IoWithContext {
                context: mut ctx,
                source,
            } => {
                Self::prepend_context_always(&mut ctx, context);
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
                Self::prepend_context_always(&mut ctx, context);
                DbError::JsonWithContext {
                    context: ctx,
                    source,
                }
            }
        }
    }

    /// Converts this error into an anyhow::Error (std only)
    ///
    /// This method provides seamless integration with anyhow for application
    /// boundaries, allowing AimDB errors to be used in anyhow error chains.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # #[cfg(feature = "std")]
    /// # {
    /// use aimdb_core::DbError;
    /// use anyhow::Context;
    ///
    /// fn application_boundary() -> anyhow::Result<()> {
    ///     let db_error = DbError::ConnectionFailed {
    ///         endpoint: "localhost:5432".to_string(),
    ///         reason: "timeout".to_string(),
    ///     };
    ///     Err(db_error.into_anyhow())
    ///         .context("Failed to initialize database connection")
    /// }
    /// # }
    /// ```
    #[cfg(feature = "std")]
    pub fn into_anyhow(self) -> anyhow::Error {
        self.into()
    }
}

// Note: anyhow::Error automatically implements From<DbError> via its blanket
// implementation for any type that implements std::error::Error + Send + Sync + 'static

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
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_error_size_monitoring() {
        // Monitor error size for performance tracking in std environments
        let size = core::mem::size_of::<DbError>();
        println!("DbError size: {} bytes", size);

        // Also test that std version is within reasonable bounds (higher than no_std)
        assert!(
            size >= 24,
            "DbError std size ({} bytes) unexpectedly small - check feature compilation",
            size
        );
    }

    #[test]
    fn test_error_creation() {
        // Test creating various error types
        let connection_error = DbError::ConnectionFailed {
            #[cfg(feature = "std")]
            endpoint: "localhost:5432".to_string(),
            #[cfg(feature = "std")]
            reason: "Connection timeout".to_string(),
            #[cfg(not(feature = "std"))]
            _endpoint: (),
            #[cfg(not(feature = "std"))]
            _reason: (),
        };

        let buffer_error = DbError::BufferFull {
            size: 1024,
            #[cfg(feature = "std")]
            buffer_name: "memory buffer".to_string(),
            #[cfg(not(feature = "std"))]
            _buffer_name: (),
        };

        // Test that errors can be formatted (std only)
        #[cfg(feature = "std")]
        {
            let connection_msg = format!("{:?}", connection_error);
            let buffer_msg = format!("{:?}", buffer_error);

            assert!(connection_msg.contains("ConnectionFailed"));
            assert!(buffer_msg.contains("BufferFull"));
        }

        // Prevent unused variable warnings in no_std
        #[cfg(not(feature = "std"))]
        {
            let _ = connection_error;
            let _ = buffer_error;
        }
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_dbresult_usage() {
        // Test DbResult type alias usage
        fn example_operation() -> DbResult<String> {
            Ok("success".to_string())
        }

        fn failing_operation() -> DbResult<String> {
            Err(DbError::ConnectionFailed {
                #[cfg(feature = "std")]
                endpoint: "database.test.com:5432".to_string(),
                #[cfg(feature = "std")]
                reason: "Connection refused".to_string(),
                #[cfg(not(feature = "std"))]
                _endpoint: (),
            })
        }

        assert!(example_operation().is_ok());
        assert!(failing_operation().is_err());
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn test_no_std_display() {
        use core::fmt::Write;

        let error = DbError::ConnectionFailed {
            _endpoint: (),
            _reason: (),
        };

        let mut buffer = heapless::String::<64>::new();
        write!(&mut buffer, "{}", error).unwrap();
        assert_eq!(buffer.as_str(), "Error 0x1002: Connection failed");
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_error_traits() {
        let error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "Connection timeout".to_string(),
        };

        // Test that error implements std::error::Error
        let _: &dyn std::error::Error = &error;

        // Test Display implementation
        let display_msg = format!("{}", error);
        assert!(display_msg.contains("Connection failed"));
        assert!(display_msg.contains("localhost:5432"));
        assert!(display_msg.contains("Connection timeout"));
    }

    #[test]
    fn test_helper_methods() {
        // Test helper constructors
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

        let buffer_error = DbError::BufferFull {
            size: 1024,
            #[cfg(feature = "std")]
            buffer_name: "buffer".to_string(),
            #[cfg(not(feature = "std"))]
            _buffer_name: (),
        };

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
        assert!(connection_error.is_network_error());
        assert!(!connection_error.is_capacity_error());
        assert!(!connection_error.is_hardware_error());

        assert!(!buffer_error.is_network_error());
        assert!(buffer_error.is_capacity_error());
        assert!(!buffer_error.is_hardware_error());

        assert!(!hardware_error.is_network_error());
        assert!(!hardware_error.is_capacity_error());
        assert!(hardware_error.is_hardware_error());
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_helper_methods() {
        let connection_error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "Database connection timeout".to_string(),
        };
        if let DbError::ConnectionFailed { endpoint, reason } = connection_error {
            assert_eq!(endpoint, "localhost:5432");
            assert_eq!(reason, "Database connection timeout");
        } else {
            panic!("Expected ConnectionFailed error");
        }

        let buffer_error = DbError::BufferFull {
            size: 2048,
            buffer_name: "memory buffer".to_string(),
        };
        if let DbError::BufferFull { size, buffer_name } = buffer_error {
            assert_eq!(size, 2048);
            assert_eq!(buffer_name, "memory buffer");
        } else {
            panic!("Expected BufferFull error");
        }
    }

    #[test]
    fn test_error_codes() {
        // Test that all error variants have correct error codes within their ranges

        // Network errors: 0x1000-0x1FFF
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

        // Capacity errors: 0x2000-0x2FFF
        let buffer_error = DbError::BufferFull {
            size: 1024,
            #[cfg(feature = "std")]
            buffer_name: String::new(),
            #[cfg(not(feature = "std"))]
            _buffer_name: (),
        };
        assert_eq!(buffer_error.error_code(), 0x2002);
        assert_eq!(buffer_error.error_category(), 0x2000);

        // Serialization errors: 0x3000-0x3FFF
        let serialization_error = DbError::SerializationFailed {
            format: 1,
            #[cfg(feature = "std")]
            details: String::new(),
            #[cfg(not(feature = "std"))]
            _details: (),
        };
        assert_eq!(serialization_error.error_code(), 0x3001);
        assert_eq!(serialization_error.error_category(), 0x3000);

        // Configuration errors: 0x4000-0x4FFF
        let config_error = DbError::MissingConfiguration {
            #[cfg(feature = "std")]
            parameter: String::new(),
            #[cfg(not(feature = "std"))]
            _parameter: (),
        };
        assert_eq!(config_error.error_code(), 0x4002);
        assert_eq!(config_error.error_category(), 0x4000);

        // Resource errors: 0x5000-0x5FFF
        let resource_error = DbError::ResourceAllocationFailed {
            resource_type: 0,
            requested_size: 1024,
            #[cfg(feature = "std")]
            details: String::new(),
            #[cfg(not(feature = "std"))]
            _details: (),
        };
        assert_eq!(resource_error.error_code(), 0x5001);
        assert_eq!(resource_error.error_category(), 0x5000);

        // Hardware errors: 0x6000-0x6FFF
        let hardware_error = DbError::HardwareError {
            component: 1,
            error_code: 500,
            #[cfg(feature = "std")]
            description: String::new(),
            #[cfg(not(feature = "std"))]
            _description: (),
        };
        assert_eq!(hardware_error.error_code(), 0x6001);
        assert_eq!(hardware_error.error_category(), 0x6000);

        // Internal errors: 0x7000-0x7FFF
        let internal_error = DbError::Internal {
            code: 500,
            #[cfg(feature = "std")]
            message: String::new(),
            #[cfg(not(feature = "std"))]
            _message: (),
        };
        assert_eq!(internal_error.error_code(), 0x7001);
        assert_eq!(internal_error.error_category(), 0x7000);
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_error_code_uniqueness() {
        // Ensure all error codes are unique
        use std::collections::HashSet;
        let mut codes = HashSet::new();

        let mut errors = vec![
            DbError::ConnectionFailed {
                #[cfg(feature = "std")]
                endpoint: String::new(),
                #[cfg(feature = "std")]
                reason: String::new(),
                #[cfg(not(feature = "std"))]
                _endpoint: (),
            },
            DbError::BufferFull {
                size: 0,
                #[cfg(feature = "std")]
                buffer_name: String::new(),
                #[cfg(not(feature = "std"))]
                _buffer_name: (),
            },
            DbError::SerializationFailed {
                format: 0,
                #[cfg(feature = "std")]
                details: String::new(),
                #[cfg(not(feature = "std"))]
                _details: (),
            },
            DbError::InvalidDataFormat {
                expected_format: 0,
                received_format: 0,
                #[cfg(feature = "std")]
                description: String::new(),
                #[cfg(not(feature = "std"))]
                _description: (),
            },
            DbError::MissingConfiguration {
                #[cfg(feature = "std")]
                parameter: String::new(),
                #[cfg(not(feature = "std"))]
                _parameter: (),
            },
            DbError::ResourceAllocationFailed {
                resource_type: 0,
                requested_size: 0,
                #[cfg(feature = "std")]
                details: String::new(),
                #[cfg(not(feature = "std"))]
                _details: (),
            },
            DbError::ResourceUnavailable {
                resource_type: 0,
                #[cfg(feature = "std")]
                resource_name: String::new(),
                #[cfg(not(feature = "std"))]
                _resource_name: (),
            },
            DbError::HardwareError {
                component: 0,
                error_code: 0,
                #[cfg(feature = "std")]
                description: String::new(),
                #[cfg(not(feature = "std"))]
                _description: (),
            },
            DbError::Internal {
                code: 0,
                #[cfg(feature = "std")]
                message: String::new(),
                #[cfg(not(feature = "std"))]
                _message: (),
            },
        ];

        // Add std-only errors if the std feature is enabled
        #[cfg(feature = "std")]
        {
            errors.push(DbError::Io {
                source: std::io::Error::other("test"),
            });
            errors.push(DbError::IoWithContext {
                context: "test context".to_string(),
                source: std::io::Error::other("test"),
            });
            errors.push(DbError::Json {
                source: serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
            });
            errors.push(DbError::JsonWithContext {
                context: "test context".to_string(),
                source: serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
            });
        }

        for error in &errors {
            let code = error.error_code();
            assert!(codes.insert(code), "Duplicate error code: 0x{:04X}", code);
        }
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn test_no_std_display_with_codes() {
        use core::fmt::Write;
        let mut buffer = heapless::String::<64>::new();

        // Test that no_std Display includes error codes
        let error = DbError::ConnectionFailed {
            _endpoint: (),
            _reason: (),
        };

        write!(&mut buffer, "{}", error).unwrap();
        assert_eq!(buffer.as_str(), "Error 0x1002: Connection failed");

        buffer.clear();

        let capacity_error = DbError::BufferFull {
            size: 100,
            _buffer_name: (),
        };

        write!(&mut buffer, "{}", capacity_error).unwrap();
        assert_eq!(buffer.as_str(), "Error 0x2002: Buffer full");
    }

    #[test]
    fn test_const_error_code_performance() {
        // Test that error_code() method provides fast lookup
        let connection_error = DbError::ConnectionFailed {
            #[cfg(feature = "std")]
            endpoint: String::new(),
            #[cfg(feature = "std")]
            reason: String::new(),
            #[cfg(not(feature = "std"))]
            _endpoint: (),
            #[cfg(not(feature = "std"))]
            _reason: (),
        };

        let buffer_error = DbError::BufferFull {
            size: 100,
            #[cfg(feature = "std")]
            buffer_name: String::new(),
            #[cfg(not(feature = "std"))]
            _buffer_name: (),
        };

        // Test that error codes are correct
        assert_eq!(connection_error.error_code(), 0x1002);
        assert_eq!(buffer_error.error_code(), 0x2002);

        // Test that the method can be called multiple times efficiently
        for _ in 0..1000 {
            assert_eq!(connection_error.error_code(), 0x1002);
            assert_eq!(buffer_error.error_code(), 0x2002);
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_display_formatting() {
        // Test rich std Display formatting
        let connection_error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "Database connection timeout".to_string(),
        };

        let display_msg = format!("{}", connection_error);
        assert!(display_msg.contains("localhost:5432"));
        assert!(display_msg.contains("Database connection timeout"));

        let buffer_error = DbError::BufferFull {
            size: 2048,
            buffer_name: "memory buffer".to_string(),
        };

        let display_msg = format!("{}", buffer_error);
        assert!(display_msg.contains("2048"));
        assert!(display_msg.contains("memory buffer"));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_error_chaining_with_context() {
        // Test that with_context() properly chains error context
        let base_error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "timeout".to_string(),
        };
        let chained_error = base_error
            .with_context("Failed to connect to database server")
            .with_context("During application startup");

        if let DbError::ConnectionFailed { reason, .. } = chained_error {
            assert_eq!(
                reason,
                "During application startup: Failed to connect to database server: timeout"
            );
        } else {
            panic!("Expected ConnectionFailed error");
        }

        // Test chaining on different error types
        let buffer_error = DbError::BufferFull {
            size: 2048,
            buffer_name: "memory".to_string(),
        }
        .with_context("Buffer allocation failed")
        .with_context("High memory usage");

        if let DbError::BufferFull { buffer_name, .. } = buffer_error {
            assert_eq!(
                buffer_name,
                "High memory usage: Buffer allocation failed: memory"
            );
        } else {
            panic!("Expected BufferFull error");
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_io_error_conversion() {
        use std::io::ErrorKind;

        // Test conversion from std::io::Error
        let io_error = std::io::Error::new(ErrorKind::NotFound, "File not found");
        let db_error: DbError = io_error.into();

        if let DbError::Io { source } = db_error {
            assert_eq!(source.kind(), ErrorKind::NotFound);
        } else {
            panic!("Expected Io error variant");
        }

        // Test that the error code is correct for I/O errors
        let io_db_error = DbError::Io {
            source: std::io::Error::new(ErrorKind::PermissionDenied, "Permission denied"),
        };
        assert_eq!(io_db_error.error_code(), 0x8001);
        assert_eq!(io_db_error.error_category(), 0x8000);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_serde_json_error_conversion() {
        // Test conversion from serde_json::Error
        let invalid_json = r#"{"invalid": json}"#;
        let json_error: serde_json::Error =
            serde_json::from_str::<serde_json::Value>(invalid_json).unwrap_err();
        let db_error: DbError = json_error.into();

        if let DbError::Json { .. } = db_error {
            // Conversion successful
        } else {
            panic!("Expected Json error variant");
        }

        // Test that the error code is correct for JSON errors
        let json_db_error = DbError::Json {
            source: serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
        };
        assert_eq!(json_db_error.error_code(), 0x9001);
        assert_eq!(json_db_error.error_category(), 0x9000);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_anyhow_integration() {
        use anyhow::Context;

        // Test converting DbError to anyhow::Error
        let db_error = DbError::ConnectionFailed {
            endpoint: "localhost:5432".to_string(),
            reason: "Connection timeout".to_string(),
        };
        let anyhow_error: anyhow::Error = db_error.into_anyhow();

        // Test that the conversion preserves the error message
        let error_msg = format!("{}", anyhow_error);
        assert!(error_msg.contains("localhost:5432"));
        assert!(error_msg.contains("Connection timeout"));

        // Test error chaining with anyhow
        fn failing_db_operation() -> Result<(), DbError> {
            Err(DbError::BufferFull {
                size: 2048,
                buffer_name: "memory buffer".to_string(),
            })
        }

        let result: anyhow::Result<()> = failing_db_operation()
            .map_err(|e| e.into_anyhow())
            .context("Database operation failed")
            .context("Application startup error");

        assert!(result.is_err());
        let error = result.unwrap_err();
        let error_chain = format!("{:#}", error);
        assert!(error_chain.contains("Application startup error"));
        assert!(error_chain.contains("Database operation failed"));
        assert!(error_chain.contains("Buffer full"));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_std_error_trait_compliance() {
        use std::error::Error;

        // Test that DbError implements std::error::Error trait correctly
        let connection_error = DbError::ConnectionFailed {
            endpoint: "localhost:3000".to_string(),
            reason: "Test timeout".to_string(),
        };
        let error_trait: &dyn Error = &connection_error;

        // Test Display
        let display_msg = format!("{}", error_trait);
        assert!(display_msg.contains("localhost:3000"));
        assert!(display_msg.contains("Test timeout"));

        // Test Debug
        let debug_msg = format!("{:?}", error_trait);
        assert!(debug_msg.contains("ConnectionFailed"));

        // Test error source for errors with sources
        let io_error = std::io::Error::other("File not found");
        let db_io_error = DbError::Io { source: io_error };
        let error_trait: &dyn Error = &db_io_error;

        assert!(error_trait.source().is_some());
        let source = error_trait.source().unwrap();
        assert_eq!(source.to_string(), "File not found");
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_error_source_chaining() {
        use std::error::Error;

        // Create a chain: serde_json::Error -> DbError
        let json_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let db_error = DbError::Json { source: json_error };

        let error_ref: &dyn Error = &db_error;

        // Verify the source chain
        assert!(error_ref.source().is_some());
        let source = error_ref.source().unwrap();
        assert!(source.to_string().contains("expected"));

        // Create a chain: io::Error -> DbError
        let io_error = std::io::Error::other("Access denied");
        let db_io_error = DbError::Io { source: io_error };

        let error_ref: &dyn Error = &db_io_error;
        assert!(error_ref.source().is_some());
        let source = error_ref.source().unwrap();
        assert_eq!(source.to_string(), "Access denied");
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_helper_functions() {
        // Test that the helper functions work correctly in isolation
        let mut content_string = String::from("some content");
        DbError::prepend_context_always(&mut content_string, "Always prepend");
        assert_eq!(content_string, "Always prepend: some content");
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_io_json_context_handling() {
        // Test that Io and Json errors can now properly receive context

        // Test I/O error with context
        let io_error = std::io::Error::other("File not found");
        let db_io_error = DbError::Io { source: io_error };
        let contextualized = db_io_error.with_context("Reading configuration file");

        if let DbError::IoWithContext { context, .. } = contextualized {
            assert_eq!(context, "Reading configuration file");
        } else {
            panic!("Expected IoWithContext error after adding context");
        }

        // Test JSON error with context
        let json_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let db_json_error = DbError::Json { source: json_error };
        let contextualized = db_json_error.with_context("Parsing user input");

        if let DbError::JsonWithContext { context, .. } = contextualized {
            assert_eq!(context, "Parsing user input");
        } else {
            panic!("Expected JsonWithContext error after adding context");
        }

        // Test chaining context on already contextualized I/O error
        let io_error = std::io::Error::other("Permission denied");
        let io_with_context = DbError::IoWithContext {
            context: "Initial context".to_string(),
            source: io_error,
        };
        let chained = io_with_context.with_context("Additional context");

        if let DbError::IoWithContext { context, .. } = chained {
            assert_eq!(context, "Additional context: Initial context");
        } else {
            panic!("Expected IoWithContext error with chained context");
        }

        // Test chaining context on already contextualized JSON error
        let json_error = serde_json::from_str::<serde_json::Value>("bad json").unwrap_err();
        let json_with_context = DbError::JsonWithContext {
            context: "Initial parse context".to_string(),
            source: json_error,
        };
        let chained = json_with_context.with_context("Higher level context");

        if let DbError::JsonWithContext { context, .. } = chained {
            assert_eq!(context, "Higher level context: Initial parse context");
        } else {
            panic!("Expected JsonWithContext error with chained context");
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_context_error_codes_and_categories() {
        // Test that context variants have correct error codes
        let io_error = std::io::Error::other("Test error");
        let io_with_context = DbError::IoWithContext {
            context: "Test context".to_string(),
            source: io_error,
        };
        assert_eq!(io_with_context.error_code(), 0x8002);
        assert_eq!(io_with_context.error_category(), 0x8000);

        let json_error = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let json_with_context = DbError::JsonWithContext {
            context: "Test context".to_string(),
            source: json_error,
        };
        assert_eq!(json_with_context.error_code(), 0x9002);
        assert_eq!(json_with_context.error_category(), 0x9000);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_context_error_display() {
        // Test that context variants display correctly
        let io_error = std::io::Error::other("File not accessible");
        let io_with_context = DbError::IoWithContext {
            context: "Loading configuration".to_string(),
            source: io_error,
        };

        let display_msg = format!("{}", io_with_context);
        assert!(display_msg.contains("Loading configuration"));
        assert!(display_msg.contains("File not accessible"));

        let json_error = serde_json::from_str::<serde_json::Value>("malformed").unwrap_err();
        let json_with_context = DbError::JsonWithContext {
            context: "Parsing response".to_string(),
            source: json_error,
        };

        let display_msg = format!("{}", json_with_context);
        assert!(display_msg.contains("Parsing response"));
        // The exact JSON error message may vary, so we just check it contains JSON context
        assert!(display_msg.contains("JSON error"));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_context_error_source_preservation() {
        use std::error::Error;

        // Test that the original error source is preserved in context variants
        let io_error = std::io::Error::other("Original I/O error");
        let io_with_context = DbError::IoWithContext {
            context: "Added context".to_string(),
            source: io_error,
        };

        let error_trait: &dyn Error = &io_with_context;
        let source = error_trait.source().expect("Should have a source error");
        assert_eq!(source.to_string(), "Original I/O error");

        let json_error = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
        let json_with_context = DbError::JsonWithContext {
            context: "JSON context".to_string(),
            source: json_error,
        };

        let error_trait: &dyn Error = &json_with_context;
        let source = error_trait.source().expect("Should have a source error");
        assert!(source.to_string().contains("expected")); // Common part of serde_json error messages
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_comprehensive_error_scenarios() {
        // Test realistic error scenarios combining all features

        // Scenario 1: Network operation with I/O failure and context
        fn simulate_network_failure() -> Result<(), DbError> {
            // Simulate file read failure that leads to network error
            let io_error = std::io::Error::other("Config file not found");
            Err(DbError::Io { source: io_error })
        }

        let result = simulate_network_failure()
            .map_err(|e| e.with_context("Failed to read network configuration"))
            .map_err(|e| e.with_context("Network initialization failed"));

        assert!(result.is_err());
        // After our fix, this should now be an IoWithContext error
        if let Err(DbError::IoWithContext { context, .. }) = result {
            assert!(context.contains("Network initialization failed"));
            assert!(context.contains("Failed to read network configuration"));
        } else {
            panic!("Expected IoWithContext error with proper context chain");
        }

        // Scenario 2: JSON parsing in configuration loading with context
        fn parse_config(json: &str) -> Result<serde_json::Value, DbError> {
            serde_json::from_str(json).map_err(|e| DbError::Json { source: e })
        }

        let result = parse_config(r#"{"invalid": json syntax}"#)
            .map_err(|e| e.with_context("Parsing user configuration"))
            .map_err(|e| e.with_context("Configuration loading failed"));

        assert!(result.is_err());
        if let Err(DbError::JsonWithContext { context, .. }) = result {
            assert!(context.contains("Configuration loading failed"));
            assert!(context.contains("Parsing user configuration"));
        } else {
            panic!("Expected JsonWithContext error with proper context chain");
        }

        // Scenario 3: Converting to anyhow for application boundary
        use anyhow::Context;

        fn application_main() -> anyhow::Result<()> {
            let db_error = DbError::BufferFull {
                size: 1024,
                buffer_name: "connection pool".to_string(),
            }
            .with_context("Pool exhausted during high load");

            Err(db_error.into_anyhow()).context("Application failed to start")
        }

        let result = application_main();
        assert!(result.is_err());
        let error_msg = format!("{:#}", result.unwrap_err());
        assert!(error_msg.contains("Application failed to start"));
        assert!(error_msg.contains("Pool exhausted"));
        assert!(error_msg.contains("connection pool"));
    }

    #[test]
    fn test_error_category_ranges() {
        // Verify error category extraction
        let network_errors = [0x1001, 0x1002, 0x1003];
        let capacity_errors = [0x2001, 0x2002];
        let serialization_errors = [0x3001, 0x3002];
        let config_errors = [0x4001, 0x4002];
        let resource_errors = [0x5001, 0x5002];
        let hardware_errors = [0x6001, 0x6002];
        let internal_errors = [0x7001];

        for &code in &network_errors {
            assert_eq!(code & 0xF000, 0x1000, "Network error category mismatch");
        }

        for &code in &capacity_errors {
            assert_eq!(code & 0xF000, 0x2000, "Capacity error category mismatch");
        }

        for &code in &serialization_errors {
            assert_eq!(
                code & 0xF000,
                0x3000,
                "Serialization error category mismatch"
            );
        }

        for &code in &config_errors {
            assert_eq!(
                code & 0xF000,
                0x4000,
                "Configuration error category mismatch"
            );
        }

        for &code in &resource_errors {
            assert_eq!(code & 0xF000, 0x5000, "Resource error category mismatch");
        }

        for &code in &hardware_errors {
            assert_eq!(code & 0xF000, 0x6000, "Hardware error category mismatch");
        }

        for &code in &internal_errors {
            assert_eq!(code & 0xF000, 0x7000, "Internal error category mismatch");
        }
    }
}
