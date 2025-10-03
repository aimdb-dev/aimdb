//! Tokio Time-related Adapter Implementations
//!
//! This module provides Tokio-specific implementations of time-related traits
//! from aimdb-core, including timestamps and sleep capabilities.

use aimdb_core::time::{SleepCapable, TimestampProvider};
use core::future::Future;
use std::time::{Duration, Instant};

use crate::TokioAdapter;

/// Implementation of TimestampProvider for TokioAdapter
///
/// Uses `std::time::Instant` to provide high-resolution timestamps
/// suitable for performance measurement and timing operations.
#[cfg(feature = "tokio-runtime")]
impl TimestampProvider for TokioAdapter {
    type Instant = Instant;

    /// Gets the current timestamp using `std::time::Instant::now()`
    ///
    /// # Returns
    /// Current timestamp as `std::time::Instant`
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_core::time::TimestampProvider;
    ///
    /// let adapter = TokioAdapter::new().unwrap();
    /// let timestamp = adapter.now();
    /// ```
    fn now(&self) -> Self::Instant {
        Instant::now()
    }
}

/// Implementation of SleepCapable for TokioAdapter
///
/// Provides non-blocking sleep capabilities using `tokio::time::sleep`,
/// which pauses execution without blocking the async runtime.
#[cfg(feature = "tokio-runtime")]
impl SleepCapable for TokioAdapter {
    type Duration = Duration;

    /// Pauses execution for the specified duration using `tokio::time::sleep`
    ///
    /// # Arguments
    /// * `duration` - How long to pause execution
    ///
    /// # Returns
    /// A future that completes after the specified duration
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_core::time::SleepCapable;
    /// use std::time::Duration;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let adapter = TokioAdapter::new().unwrap();
    /// adapter.sleep(Duration::from_millis(100)).await;
    /// # }
    /// ```
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }
}
