//! Tokio-specific error handling support
//!
//! This module provides traits and implementations that add Tokio
//! and std async runtime specific functionality to AimDB's core error types.
//!
//! Tokio is a std async runtime, so this adapter always works in std mode
//! and uses the std field names from DbError with rich error descriptions.

use aimdb_core::DbError;
use std::time::Duration;

// Tokio Error Code Base Values
const RUNTIME_ERROR_BASE: u16 = 0x7100;

// Component IDs for Tokio runtime components
const TASK_COMPONENT_ID: u8 = 12;

/// Trait that provides Tokio-specific error constructors for DbError
///
/// This trait provides async runtime-specific error creation methods without requiring
/// the core AimDB crate to depend on tokio directly.
pub trait TokioErrorSupport {
    /// Creates a runtime error for Tokio environments (error codes 0x7100-0x71FF)
    ///
    /// # Returns
    /// `DbError::InternalError { error_code: 0x7100 | code, description: "Runtime error: <msg>" }`
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// let runtime_error = DbError::from_runtime_error(0x01, "Runtime not available");
    /// ```
    fn from_runtime_error(code: u8, description: &str) -> Self;

    /// Creates a timeout error for Tokio environments (error codes 0x7200-0x72FF)
    ///
    /// # Returns
    /// `DbError::NetworkError { error_code: 0x7200 | code, endpoint: "timeout", description: "Operation timed out after <duration>" }`
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// use std::time::Duration;
    /// let timeout_error = DbError::from_timeout_error(0x01, Duration::from_millis(5000));
    /// ```
    fn from_timeout_error(_code: u8, timeout: Duration) -> Self;

    /// Creates a task error for Tokio environments (error codes 0x7300-0x73FF)
    ///
    /// # Returns
    /// `DbError::ResourceError { error_code: 0x7300 | code, resource_type: "task", description: "Task error: <msg>" }`
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// let task_error = DbError::from_task_error(0x02, "Task cancelled");
    /// ```
    fn from_task_error(_code: u8, description: &str) -> Self;

    /// Creates an I/O error for Tokio environments (error codes 0x7400-0x74FF)
    ///
    /// # Returns
    /// `DbError::IoError { error_code: 0x7400 | code, description: "I/O error: <msg>" }`
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// let io_error = DbError::from_io_error(0x01, "Connection refused");
    /// ```
    fn from_io_error(code: u8, description: &str) -> Self;

    /// Converts a tokio::time::error::Elapsed to DbError
    ///
    /// # Returns
    /// `DbError::NetworkError` with timeout-specific error code and description
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// // let elapsed = tokio::time::timeout(Duration::from_millis(100), future).await.unwrap_err();
    /// // let db_error = DbError::from_elapsed_error(elapsed);
    /// ```
    fn from_elapsed_error(error: tokio::time::error::Elapsed) -> Self;

    /// Converts a tokio::task::JoinError to DbError
    ///
    /// # Returns
    /// `DbError::ResourceError` with task-specific error code and description
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// // let join_error = task_handle.await.unwrap_err();
    /// // let db_error = DbError::from_join_error(join_error);
    /// ```
    fn from_join_error(error: tokio::task::JoinError) -> Self;

    /// Converts a std::io::Error to DbError
    ///
    /// # Returns
    /// `DbError::IoError` with I/O-specific error code and description
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// use std::io;
    /// let io_error = io::Error::new(io::ErrorKind::ConnectionRefused, "Connection failed");
    /// let db_error = DbError::from_std_io_error(io_error);
    /// ```
    fn from_std_io_error(error: std::io::Error) -> Self;
}

impl TokioErrorSupport for DbError {
    /// Creates a runtime error for Tokio environments (error codes 0x7100-0x71FF)
    fn from_runtime_error(code: u8, description: &str) -> Self {
        DbError::Internal {
            code: (RUNTIME_ERROR_BASE | (code as u16)) as u32,
            message: format!("Runtime error: {}", description),
        }
    }

    /// Creates a timeout error for Tokio environments (error codes 0x7200-0x72FF)
    fn from_timeout_error(_code: u8, timeout: Duration) -> Self {
        DbError::ConnectionFailed {
            endpoint: "timeout".to_string(),
            reason: format!("Operation timed out after {}ms", timeout.as_millis()),
        }
    }

    /// Creates a task error for Tokio environments (error codes 0x7300-0x73FF)
    fn from_task_error(_code: u8, description: &str) -> Self {
        DbError::ResourceUnavailable {
            resource_type: TASK_COMPONENT_ID,
            resource_name: format!("task: {}", description),
        }
    }

    /// Creates an I/O error for Tokio environments (error codes 0x7400-0x74FF)
    fn from_io_error(code: u8, description: &str) -> Self {
        DbError::IoWithContext {
            context: format!("Tokio I/O error ({})", code),
            source: std::io::Error::other(description),
        }
    }

    /// Converts a tokio::time::error::Elapsed to DbError
    fn from_elapsed_error(_error: tokio::time::error::Elapsed) -> Self {
        DbError::ConnectionFailed {
            endpoint: "timeout".to_string(),
            reason: "Operation timed out".to_string(),
        }
    }

    /// Converts a tokio::task::JoinError to DbError
    fn from_join_error(error: tokio::task::JoinError) -> Self {
        if error.is_cancelled() {
            DbError::ResourceUnavailable {
                resource_type: TASK_COMPONENT_ID,
                resource_name: "Task was cancelled".to_string(),
            }
        } else if error.is_panic() {
            DbError::Internal {
                code: (RUNTIME_ERROR_BASE | 0x02) as u32,
                message: "Task panicked".to_string(),
            }
        } else {
            DbError::ResourceUnavailable {
                resource_type: TASK_COMPONENT_ID,
                resource_name: format!("Task join error: {}", error),
            }
        }
    }

    /// Converts a std::io::Error to DbError
    fn from_std_io_error(error: std::io::Error) -> Self {
        use std::io::ErrorKind;

        let context = match error.kind() {
            ErrorKind::NotFound => "File not found",
            ErrorKind::PermissionDenied => "Permission denied",
            ErrorKind::ConnectionRefused => "Connection refused",
            ErrorKind::ConnectionReset => "Connection reset",
            ErrorKind::ConnectionAborted => "Connection aborted",
            ErrorKind::NotConnected => "Not connected",
            ErrorKind::AddrInUse => "Address in use",
            ErrorKind::AddrNotAvailable => "Address not available",
            ErrorKind::BrokenPipe => "Broken pipe",
            ErrorKind::AlreadyExists => "Already exists",
            ErrorKind::WouldBlock => "Would block",
            ErrorKind::InvalidInput => "Invalid input",
            ErrorKind::InvalidData => "Invalid data",
            ErrorKind::TimedOut => "Timed out",
            ErrorKind::WriteZero => "Write zero",
            ErrorKind::Interrupted => "Interrupted",
            ErrorKind::Unsupported => "Unsupported",
            ErrorKind::UnexpectedEof => "Unexpected EOF",
            ErrorKind::OutOfMemory => "Out of memory",
            _ => "I/O error",
        };

        DbError::IoWithContext {
            context: context.to_string(),
            source: error,
        }
    }
}

/// Converter functions for Tokio runtime errors to DbError
pub struct TokioErrorConverter;

impl TokioErrorConverter {
    /// Converts a tokio::time::error::Elapsed to DbError
    pub fn from_elapsed(error: tokio::time::error::Elapsed) -> DbError {
        DbError::from_elapsed_error(error)
    }

    /// Converts a tokio::task::JoinError to DbError
    pub fn from_join_error(error: tokio::task::JoinError) -> DbError {
        DbError::from_join_error(error)
    }

    /// Converts a std::io::Error to DbError
    pub fn from_io_error(error: std::io::Error) -> DbError {
        DbError::from_std_io_error(error)
    }

    /// Creates a timeout error with duration context
    pub fn timeout_error(timeout: Duration) -> DbError {
        DbError::from_timeout_error(0x01, timeout)
    }

    /// Creates a runtime unavailable error
    pub fn runtime_unavailable() -> DbError {
        DbError::from_runtime_error(0x01, "Tokio runtime not available")
    }

    /// Creates a task cancelled error
    pub fn task_cancelled() -> DbError {
        DbError::from_task_error(0x01, "Task was cancelled")
    }
}

// Note: From trait implementations are not provided due to Rust's orphan rules.
// Use TokioErrorConverter methods or trait methods instead.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_tokio_runtime_error_constructors() {
        // Test Tokio-specific error constructors

        // Test runtime error constructor (0x7100-0x71FF range)
        let runtime_error = DbError::from_runtime_error(0x01, "Runtime initialization failed");
        if let DbError::Internal { code, message } = runtime_error {
            assert_eq!(code, (RUNTIME_ERROR_BASE | 0x01) as u32);
            assert!(message.contains("Runtime initialization failed"));
        } else {
            panic!("Expected Internal variant");
        }

        // Test timeout error constructor (0x7200-0x72FF range)
        let timeout = Duration::from_millis(5000);
        let timeout_error = DbError::from_timeout_error(0x01, timeout);
        if let DbError::ConnectionFailed { endpoint, reason } = timeout_error {
            assert_eq!(endpoint, "timeout");
            assert!(reason.contains("5000ms"));
        } else {
            panic!("Expected ConnectionFailed variant");
        }

        // Test task error constructor (0x7300-0x73FF range)
        let task_error = DbError::from_task_error(0x02, "Task execution failed");
        if let DbError::ResourceUnavailable {
            resource_type,
            resource_name,
        } = task_error
        {
            assert_eq!(resource_type, TASK_COMPONENT_ID);
            assert!(resource_name.contains("Task execution failed"));
        } else {
            panic!("Expected ResourceUnavailable variant");
        }

        // Test I/O error constructor (0x7400-0x74FF range)
        let io_error = DbError::from_io_error(0x01, "Connection failed");
        if let DbError::IoWithContext { context, source } = io_error {
            assert!(context.contains("Tokio I/O error"));
            assert!(source.to_string().contains("Connection failed"));
        } else {
            panic!("Expected IoWithContext variant");
        }
    }

    #[test]
    fn test_std_io_error_conversions() {
        use std::io::{Error, ErrorKind};

        // Test various I/O error kinds
        let not_found = Error::new(ErrorKind::NotFound, "file.txt");
        let db_error = DbError::from_std_io_error(not_found);
        if let DbError::IoWithContext { context, .. } = db_error {
            assert_eq!(context, "File not found");
        } else {
            panic!("Expected IoWithContext variant");
        }

        let permission_denied = Error::new(ErrorKind::PermissionDenied, "access denied");
        let db_error = DbError::from_std_io_error(permission_denied);
        if let DbError::IoWithContext { context, .. } = db_error {
            assert_eq!(context, "Permission denied");
        } else {
            panic!("Expected IoWithContext variant");
        }

        let connection_refused = Error::new(ErrorKind::ConnectionRefused, "localhost:8080");
        let db_error = DbError::from_std_io_error(connection_refused);
        if let DbError::IoWithContext { context, .. } = db_error {
            assert_eq!(context, "Connection refused");
        } else {
            panic!("Expected IoWithContext variant");
        }
    }

    #[test]
    fn test_tokio_error_categorization() {
        // Test that Tokio runtime errors are properly categorized
        let runtime_error = DbError::from_runtime_error(0x00, "test");
        assert!(matches!(runtime_error, DbError::Internal { .. }));

        let timeout_error = DbError::from_timeout_error(0x00, Duration::from_millis(1000));
        assert!(matches!(timeout_error, DbError::ConnectionFailed { .. }));

        let task_error = DbError::from_task_error(0x00, "test");
        assert!(matches!(task_error, DbError::ResourceUnavailable { .. }));

        let io_error = DbError::from_io_error(0x00, "test");
        assert!(matches!(io_error, DbError::IoWithContext { .. }));
    }

    #[test]
    fn test_converter_functions() {
        // Test TokioErrorConverter functions
        let timeout_error = TokioErrorConverter::timeout_error(Duration::from_millis(3000));
        if let DbError::ConnectionFailed { reason, .. } = timeout_error {
            assert!(reason.contains("3000ms"));
        } else {
            panic!("Expected ConnectionFailed variant");
        }

        let runtime_error = TokioErrorConverter::runtime_unavailable();
        if let DbError::Internal { message, .. } = runtime_error {
            assert!(message.contains("runtime not available"));
        } else {
            panic!("Expected Internal variant");
        }

        let cancelled_error = TokioErrorConverter::task_cancelled();
        if let DbError::ResourceUnavailable { resource_name, .. } = cancelled_error {
            assert!(resource_name.contains("cancelled"));
        } else {
            panic!("Expected ResourceUnavailable variant");
        }
    }

    #[test]
    fn test_from_trait_implementations() {
        // Test automatic conversions using From trait
        use std::io::{Error, ErrorKind};

        let io_error = Error::new(ErrorKind::ConnectionRefused, "test connection");
        let db_error = TokioErrorConverter::from_io_error(io_error);
        if let DbError::IoWithContext { context, .. } = db_error {
            assert_eq!(context, "Connection refused");
        } else {
            panic!("Expected IoWithContext variant");
        }
    }

    #[tokio::test]
    async fn test_elapsed_error_conversion() {
        // Test actual Elapsed error conversion
        use tokio::time::{sleep, timeout, Duration};

        let result = timeout(Duration::from_millis(10), sleep(Duration::from_millis(100))).await;
        match result {
            Err(elapsed) => {
                let db_error = DbError::from_elapsed_error(elapsed);
                if let DbError::ConnectionFailed { reason, .. } = db_error {
                    assert!(reason.contains("timed out"));
                } else {
                    panic!("Expected ConnectionFailed variant");
                }
            }
            Ok(_) => panic!("Expected timeout error"),
        }
    }

    #[tokio::test]
    async fn test_join_error_conversion() {
        // Test JoinError conversions for different error types

        // Test cancelled task
        let handle = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        });
        handle.abort();

        let result = handle.await;
        match result {
            Err(join_error) => {
                let db_error = DbError::from_join_error(join_error);
                if let DbError::ResourceUnavailable { resource_name, .. } = db_error {
                    assert!(resource_name.contains("cancelled"));
                } else {
                    panic!("Expected ResourceUnavailable variant for cancelled task");
                }
            }
            Ok(_) => panic!("Expected join error"),
        }
    }

    #[test]
    fn test_practical_error_usage_examples() {
        // Practical examples showing how Tokio errors are created and used

        // Example 1: Database connection timeout
        let connection_timeout = DbError::from_timeout_error(0x01, Duration::from_millis(5000));
        match connection_timeout {
            DbError::ConnectionFailed { endpoint, reason } => {
                assert_eq!(endpoint, "timeout");
                assert!(reason.contains("5000ms"));
                // In practice: retry connection, use backup endpoint, or return cached data
            }
            _ => panic!("Expected ConnectionFailed"),
        }

        // Example 2: Runtime initialization failure
        let runtime_failure =
            DbError::from_runtime_error(0x02, "Failed to initialize async runtime");
        match runtime_failure {
            DbError::Internal { code, message } => {
                assert_eq!(code, 0x7102); // RUNTIME_ERROR_BASE (0x7100) | 0x02
                assert!(message.contains("Failed to initialize async runtime"));
                // In practice: fallback to sync operations, restart service, or fail gracefully
            }
            _ => panic!("Expected Internal"),
        }

        // Example 3: Task cancellation handling
        let task_cancelled = DbError::from_task_error(0x01, "Background sync task was cancelled");
        match task_cancelled {
            DbError::ResourceUnavailable {
                resource_type,
                resource_name,
            } => {
                assert_eq!(resource_type, TASK_COMPONENT_ID);
                assert!(resource_name.contains("cancelled"));
                // In practice: reschedule task, notify user, or update sync status
            }
            _ => panic!("Expected ResourceUnavailable"),
        }

        // Example 4: Network I/O failure
        let network_io_error = DbError::from_io_error(0x03, "TCP connection reset by peer");
        match network_io_error {
            DbError::IoWithContext { context, source } => {
                assert!(context.contains("Tokio I/O error"));
                assert!(source.to_string().contains("TCP connection reset"));
                // In practice: reconnect, switch to backup connection, or queue for retry
            }
            _ => panic!("Expected IoWithContext"),
        }
    }

    #[test]
    fn test_error_context_preservation() {
        // Test that error context and descriptions are preserved properly

        let timeout_error = DbError::from_timeout_error(0x05, Duration::from_millis(2500));

        if let DbError::ConnectionFailed { reason, .. } = timeout_error {
            assert!(reason.contains("2500ms"));
            assert!(reason.contains("timed out"));
            // Verify rich error context is available in std environment
        } else {
            panic!("Expected ConnectionFailed with rich description");
        }

        // Test I/O error context preservation
        use std::io::{Error, ErrorKind};
        let detailed_io_error = Error::new(
            ErrorKind::ConnectionRefused,
            "Database server at localhost:5432 refused connection",
        );
        let db_error = DbError::from_std_io_error(detailed_io_error);

        if let DbError::IoWithContext { context, source } = db_error {
            assert!(context.contains("Connection refused"));
            assert!(source.to_string().contains("localhost:5432"));
        } else {
            panic!("Expected IoWithContext with detailed context");
        }
    }
}
