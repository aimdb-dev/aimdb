//! Observability and metrics for producer-consumer patterns
//!
//! Provides lock-free call tracking for producers and consumers,
//! enabling runtime inspection and debugging.

use core::fmt::Debug;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(not(feature = "std"))]
use alloc::sync::Arc;
#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(not(feature = "std"))]
extern crate alloc;

// For mutex, we use different strategies based on std/no_std
#[cfg(feature = "std")]
use std::sync::Mutex;

#[cfg(not(feature = "std"))]
use spin::Mutex;

/// Statistics tracker for producer/consumer calls
///
/// Tracks the number of invocations and the last argument value
/// in a lock-free manner using atomic operations where possible.
///
/// # Design
///
/// - Call counting uses `AtomicU32` for embedded compatibility
/// - Last argument storage uses minimal locking (spin::Mutex in no_std)
/// - Generic over argument type for type safety
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::experimental::CallStats;
///
/// let stats = CallStats::<i32>::new();
/// stats.record(&42);
/// stats.record(&100);
///
/// assert_eq!(stats.calls(), 2);
/// assert_eq!(stats.last_arg(), Some(100));
/// ```
#[derive(Debug)]
pub struct CallStats<T: Debug + Clone + Send> {
    /// Atomic counter for number of calls (u32 for embedded compatibility)
    calls: AtomicU32,

    /// Last argument value (requires minimal locking)
    last_arg: Mutex<Option<T>>,
}

impl<T: Debug + Clone + Send> CallStats<T> {
    /// Creates a new call statistics tracker
    ///
    /// # Returns
    /// A new `CallStats<T>` with zero calls and no recorded argument
    pub fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
            last_arg: Mutex::new(None),
        }
    }

    /// Records a function call with the given argument
    ///
    /// Updates both the call counter and stores the argument value.
    ///
    /// # Arguments
    /// * `arg` - The argument to record
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// stats.record(&SensorData { temp: 23.5 });
    /// ```
    pub fn record(&self, arg: &T) {
        // Increment call counter atomically
        self.calls.fetch_add(1, Ordering::Relaxed);

        // Store last argument (requires brief lock)
        #[cfg(feature = "std")]
        {
            *self.last_arg.lock().unwrap() = Some(arg.clone());
        }

        #[cfg(not(feature = "std"))]
        {
            *self.last_arg.lock() = Some(arg.clone());
        }
    }

    /// Returns the total number of calls recorded
    ///
    /// # Returns
    /// The call count as `u64`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let count = stats.calls();
    /// println!("Function called {} times", count);
    /// ```
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed) as u64
    }

    /// Returns the last recorded argument value
    ///
    /// # Returns
    /// `Some(T)` if at least one call has been recorded, `None` otherwise
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(last) = stats.last_arg() {
    ///     println!("Last value: {:?}", last);
    /// }
    /// ```
    pub fn last_arg(&self) -> Option<T> {
        #[cfg(feature = "std")]
        {
            self.last_arg.lock().unwrap().clone()
        }

        #[cfg(not(feature = "std"))]
        {
            self.last_arg.lock().clone()
        }
    }

    /// Resets the statistics to initial state
    ///
    /// Useful for testing or when reusing statistics trackers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// stats.reset();
    /// assert_eq!(stats.calls(), 0);
    /// assert_eq!(stats.last_arg(), None);
    /// ```
    pub fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);

        #[cfg(feature = "std")]
        {
            *self.last_arg.lock().unwrap() = None;
        }

        #[cfg(not(feature = "std"))]
        {
            *self.last_arg.lock() = None;
        }
    }
}

impl<T: Debug + Clone + Send> Default for CallStats<T> {
    fn default() -> Self {
        Self::new()
    }
}

// Wrap in Arc for easy sharing
impl<T: Debug + Clone + Send> CallStats<T> {
    /// Creates a new `Arc<CallStats<T>>` for efficient sharing
    ///
    /// # Returns
    /// An `Arc` wrapping a new `CallStats<T>`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let stats = CallStats::<SensorData>::shared();
    /// // Can be cloned and shared across tasks
    /// let stats2 = stats.clone();
    /// ```
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_stats_basic() {
        let stats = CallStats::<i32>::new();

        assert_eq!(stats.calls(), 0);
        assert_eq!(stats.last_arg(), None);

        stats.record(&42);
        assert_eq!(stats.calls(), 1);
        assert_eq!(stats.last_arg(), Some(42));

        stats.record(&100);
        assert_eq!(stats.calls(), 2);
        assert_eq!(stats.last_arg(), Some(100));
    }

    #[test]
    fn test_call_stats_reset() {
        let stats = CallStats::<i32>::new();

        stats.record(&42);
        stats.record(&100);
        assert_eq!(stats.calls(), 2);

        stats.reset();
        assert_eq!(stats.calls(), 0);
        assert_eq!(stats.last_arg(), None);
    }

    #[test]
    fn test_call_stats_shared() {
        let stats = CallStats::<i32>::shared();
        let stats2 = stats.clone();

        stats.record(&42);
        assert_eq!(stats2.calls(), 1);
        assert_eq!(stats2.last_arg(), Some(42));
    }

    #[derive(Debug, Clone)]
    struct TestData {
        value: i32,
    }

    #[test]
    fn test_call_stats_custom_type() {
        let stats = CallStats::<TestData>::new();

        let data = TestData { value: 42 };
        stats.record(&data);

        assert_eq!(stats.calls(), 1);
        let last = stats.last_arg().unwrap();
        assert_eq!(last.value, 42);
    }
}
