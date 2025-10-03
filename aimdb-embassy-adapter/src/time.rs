//! Embassy Time Integration for AimDB
//!
//! This module provides Embassy-specific implementations of AimDB's time traits,
//! enabling embedded applications to use timing functionality with Embassy's
//! time driver and timer primitives.

use aimdb_core::time::{SleepCapable, TimestampProvider};
use core::future::Future;

#[cfg(feature = "embassy-time")]
use embassy_time::{Duration, Instant, Timer};

#[cfg(feature = "tracing")]
use tracing::{debug, trace};

/// Time provider implementing both timestamp and sleep capabilities
///
/// This provider uses Embassy's time driver to provide both timestamp and sleep
/// functionality in embedded environments. It integrates with Embassy's timer
/// system and works without requiring std.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(all(not(feature = "std"), feature = "embassy-time"))]
/// # {
/// use aimdb_embassy_adapter::time::TimeProvider;
/// use aimdb_core::time::{TimestampProvider, SleepCapable};
/// use embassy_time::Duration;
///
/// # async fn example() {
/// let provider = TimeProvider::new();
///
/// // Use as timestamp provider
/// let timestamp = provider.now();
///
/// // Use as sleep provider
/// provider.sleep(Duration::from_millis(50)).await;
/// # }
/// # }
/// ```
#[cfg(feature = "embassy-time")]
#[derive(Debug, Clone, Copy)]
pub struct TimeProvider;

#[cfg(feature = "embassy-time")]
impl TimeProvider {
    /// Creates a new time provider
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(feature = "embassy-time")]
impl Default for TimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "embassy-time")]
impl TimestampProvider for TimeProvider {
    type Instant = Instant;

    fn now(&self) -> Self::Instant {
        #[cfg(feature = "tracing")]
        trace!("Getting timestamp");

        Instant::now()
    }
}

#[cfg(feature = "embassy-time")]
impl SleepCapable for TimeProvider {
    type Duration = Duration;

    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        #[cfg(feature = "tracing")]
        debug!(duration_ms = duration.as_millis(), "Starting sleep");

        Timer::after(duration)
    }
}

/// Utility functions for Embassy time operations
///
/// These functions provide convenient ways to perform common time-based
/// operations using Embassy's time primitives.
pub mod utils {
    #[cfg(feature = "embassy-time")]
    use super::*;

    /// Measures execution time using the time provider
    ///
    /// This is a convenience function that uses Embassy's timing system
    /// to measure how long an async operation takes to complete.
    ///
    /// # Returns
    /// A tuple containing (result, start_instant, end_instant)
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(all(not(feature = "std"), feature = "embassy-time"))]
    /// # {
    /// use aimdb_embassy_adapter::time::utils;
    ///
    /// # async fn example() {
    /// let (result, start, end) = utils::measure_async(async {
    ///     // Some async work
    ///     42
    /// }).await;
    ///
    /// // Calculate duration if needed
    /// // let duration = end - start; // This depends on Embassy's Instant implementation
    /// # }
    /// # }
    /// ```
    #[cfg(feature = "embassy-time")]
    pub async fn measure_async<F, T>(operation: F) -> (T, Instant, Instant)
    where
        F: Future<Output = T>,
    {
        let provider = TimeProvider::new();
        aimdb_core::time::utils::measure_async(&provider, operation).await
    }

    /// Creates a timeout error for time operations
    ///
    /// This provides a timeout error that can be used when operations
    /// exceed their time limits in embedded environments.
    pub fn create_timeout_error() -> aimdb_core::DbError {
        aimdb_core::time::utils::create_timeout_error("Embassy operation")
    }
}
