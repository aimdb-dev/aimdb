//! Time and timing utilities for AimDB
//!
//! This module provides time-related traits and utilities for runtime adapters,
//! including timestamping, delays, and scheduling operations.

use core::future::Future;

/// Trait for adapters that provide current time information
///
/// This trait enables adapters to provide timing information which can be
/// useful for timestamping, performance measurement, and scheduling.
///
/// # Availability
/// - **Tokio environments**: Uses `std::time::Instant`
/// - **Embassy environments**: Uses `embassy_time::Instant` (when embassy-time feature enabled)
/// - **Basic environments**: May not be available depending on platform
pub trait TimestampProvider {
    /// Type representing an instant in time for this runtime
    type Instant;

    /// Gets the current timestamp according to the runtime's time source
    ///
    /// # Returns
    /// Current timestamp as the runtime's Instant type
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_core::time::TimestampProvider;
    ///
    /// fn log_operation<T: TimestampProvider>(provider: &T) {
    ///     let _timestamp = provider.now();
    ///     // Use timestamp for logging, performance measurement, etc.
    ///     println!("Operation completed");
    /// }
    /// ```
    fn now(&self) -> Self::Instant;
}

/// Trait for adapters that support sleep/delay operations
///
/// This trait provides the capability to pause execution for a specified duration,
/// useful for implementing delays, rate limiting, and timing-based control flow.
///
/// # Availability
/// - **Tokio environments**: Full support with `tokio::time::sleep()`
/// - **Embassy environments**: Available when `embassy-time` feature is enabled
/// - **Basic environments**: Not available
pub trait SleepCapable {
    /// Type representing a duration for this runtime
    type Duration;

    /// Pauses execution for the specified duration
    ///
    /// This method provides a sleep/delay functionality that pauses the current
    /// execution context without blocking other tasks.
    ///
    /// # Arguments
    /// * `duration` - How long to pause execution
    ///
    /// # Returns
    /// A future that completes after the specified duration
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_core::time::SleepCapable;
    /// use std::time::Duration;
    ///
    /// async fn rate_limited_operation<S: SleepCapable<Duration = Duration>>(
    ///     sleeper: &S
    /// ) {
    ///     println!("Starting operation...");
    ///     sleeper.sleep(Duration::from_secs(1)).await;
    ///     println!("Operation completed after delay");
    /// }
    /// ```
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send;
}

/// Utility functions for time-based operations
///
/// This module provides convenience functions that work with the timing traits
/// to provide common time-based functionality across different platforms.
pub mod utils {
    use super::*;

    /// Measures the execution time of an async operation using a timestamp provider
    ///
    /// This function works in both `std` and `no_std` environments by using the
    /// provided `TimestampProvider` for timing measurements. The duration calculation
    /// depends on the timestamp provider's `Instant` type supporting subtraction.
    pub async fn measure_async<F, T, P>(provider: &P, operation: F) -> (T, P::Instant, P::Instant)
    where
        F: Future<Output = T>,
        P: TimestampProvider,
    {
        let start = provider.now();
        let result = operation.await;
        let end = provider.now();
        (result, start, end)
    }

    /// Creates a generic timeout error for operations that exceed their time limit
    ///
    /// This utility function provides a standard way to create timeout errors
    /// without depending on specific runtime implementations.
    pub fn create_timeout_error(_operation_name: &str) -> crate::DbError {
        crate::DbError::ConnectionFailed {
            #[cfg(feature = "std")]
            endpoint: "timeout".to_string(),
            #[cfg(feature = "std")]
            reason: format!("{} operation timed out", _operation_name),
            #[cfg(not(feature = "std"))]
            _endpoint: (),
            #[cfg(not(feature = "std"))]
            _reason: (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "std")]
    mod std_tests {
        use super::*;
        use std::time::Instant;

        // Mock providers for testing
        struct MockTimestampProvider;

        impl TimestampProvider for MockTimestampProvider {
            type Instant = Instant;

            fn now(&self) -> Self::Instant {
                Instant::now()
            }
        }

        #[test]
        fn test_timestamp_provider_trait() {
            let provider = MockTimestampProvider;
            let timestamp1 = provider.now();

            // Timestamps should be valid Instant values
            let timestamp2 = provider.now();

            // Second timestamp should be greater than or equal to first
            assert!(timestamp2 >= timestamp1);
        }

        #[test]
        fn test_timeout_error_creation() {
            let error = utils::create_timeout_error("database query");

            // Test error format for std feature
            #[cfg(feature = "std")]
            {
                let error_string = format!("{}", error);
                assert!(error_string.contains("database query operation timed out"));
            }
        }

        #[test]
        fn test_measure_async_trait_based() {
            let provider = MockTimestampProvider;

            // Test that we can measure with the trait-based approach
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (result, start, end) = runtime.block_on(utils::measure_async(&provider, async {
                // Simulate work with a small computation
                let mut sum = 0;
                for i in 0..1000 {
                    sum += i;
                }
                sum
            }));

            assert_eq!(result, 499500); // Sum of 0..1000
                                        // End time should be greater than or equal to start time
            assert!(end >= start);
        }
    }

    #[cfg(not(feature = "std"))]
    mod no_std_tests {
        use super::{utils, TimestampProvider};

        // Mock timestamp provider for no_std testing
        struct MockNoStdTimestampProvider {
            counter: core::sync::atomic::AtomicU64,
        }

        impl MockNoStdTimestampProvider {
            fn new() -> Self {
                Self {
                    counter: core::sync::atomic::AtomicU64::new(0),
                }
            }
        }

        impl TimestampProvider for MockNoStdTimestampProvider {
            type Instant = u64;

            fn now(&self) -> Self::Instant {
                self.counter
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
            }
        }

        #[test]
        fn test_timeout_error_creation_no_std() {
            let _error = utils::create_timeout_error("operation");
            // In no_std mode, we just verify the error can be created
            // without panicking
        }

        #[test]
        fn test_measure_async_no_std() {
            // Test that measure_async works in no_std with a simple timestamp provider
            let provider = MockNoStdTimestampProvider::new();

            // Since we can't use tokio in no_std, we'll test the function signature
            // and basic functionality without actually running async code
            let future = async {
                // Simulate some work
                42
            };

            // This mainly tests that the function compiles and accepts the right types
            let _ = utils::measure_async(&provider, future);
        }
    }
}
