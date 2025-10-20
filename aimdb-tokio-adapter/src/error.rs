//! Tokio-specific error handling support
//!
//! This module provides traits and implementations that add Tokio
//! and std async runtime specific functionality to AimDB's core error types.
//!
//! Tokio is a std async runtime, so this adapter always works in std mode
//! and uses the std field names from DbError with rich error descriptions.

use aimdb_core::DbError;

// Tokio Error Code Base Values
const RUNTIME_ERROR_BASE: u16 = 0x7100;

// Component IDs for Tokio runtime components
const TASK_COMPONENT_ID: u8 = 12;

/// Trait that provides Tokio-specific error conversions for DbError
///
/// This trait provides essential async runtime error conversions without requiring
/// the core AimDB crate to depend on tokio directly.
///
/// # Production Usage
/// Only includes methods actually used by the Tokio adapter runtime.
/// For test-only error construction, use `DbError` constructors directly.
pub trait TokioErrorSupport {
    /// Converts a tokio::task::JoinError to DbError
    ///
    /// Used when spawned tasks fail or are cancelled. This is the primary
    /// error conversion method used in production code.
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_tokio_adapter::TokioErrorSupport;
    /// // let join_error = task_handle.await.unwrap_err();
    /// // let db_error = DbError::from_join_error(join_error);
    /// ```
    fn from_join_error(error: tokio::task::JoinError) -> Self;
}

impl TokioErrorSupport for DbError {
    /// Converts a tokio::task::JoinError to DbError.
    ///
    /// This is the only production-critical error conversion for the Tokio adapter.
    /// Task join errors occur when spawned tasks fail, and require special handling
    /// to distinguish between cancellation, panics, and regular task failures.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_join_error_conversion_cancelled() {
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

    #[tokio::test]
    async fn test_join_error_conversion_panic() {
        // Test panicked task
        let handle = tokio::spawn(async { panic!("test panic") });

        let result = handle.await;
        match result {
            Err(join_error) => {
                let db_error = DbError::from_join_error(join_error);
                if let DbError::Internal { code, message } = db_error {
                    assert_eq!(code, (RUNTIME_ERROR_BASE | 0x02) as u32);
                    assert!(message.contains("panicked"));
                } else {
                    panic!("Expected Internal variant for panicked task");
                }
            }
            Ok(_) => panic!("Expected join error"),
        }
    }

    #[test]
    fn test_error_categorization() {
        // Simulate a cancelled task error (using abort)
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        handle.abort();

        let result = rt.block_on(handle);
        if let Err(join_error) = result {
            let db_error = DbError::from_join_error(join_error);
            assert!(matches!(db_error, DbError::ResourceUnavailable { .. }));
        }
    }
}
