//! Async function wrapper with automatic call tracking
//!
//! **Internal module**: These types are used internally by the producer-consumer
//! system and are not part of the public API.
//!
//! Provides type-erased async functions that automatically record
//! execution statistics via `CallStats`.

use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

use crate::emitter::Emitter;
use crate::metrics::CallStats;

/// Type alias for boxed future returning unit
type BoxFutureUnit = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Type-erased async function
///
/// Stores a function that takes `(Emitter, T)` and returns a future.
/// Wrapped in Arc for sharing across the closure.
type ErasedAsyncFn<T> = Arc<dyn Fn(Emitter, T) -> BoxFutureUnit + Send + Sync + 'static>;

/// Async function wrapper with automatic call tracking
///
/// **Internal type**: Used by the producer-consumer system to track function calls.
/// Not re-exported from crate root. Access statistics via `Database::producer_stats()`
/// and `Database::consumer_stats()` instead of using this type directly.
///
/// Wraps an async function and automatically records call statistics
/// including invocation count and last argument value.
///
/// # Type Parameters
/// * `T` - The argument type (must be `Send + 'static + Debug + Clone`)
///
/// # Design
///
/// - Type-erases async functions for storage in collections
/// - Automatically records each call in `CallStats`
/// - Zero-cost abstraction when not inspecting stats
pub struct TrackedAsyncFn<T: Send + 'static + Debug + Clone> {
    /// The type-erased function (wrapped in Arc for cloning into closure)
    func: ErasedAsyncFn<T>,

    /// Statistics tracker
    stats: Arc<CallStats<T>>,
}

impl<T: Send + 'static + Debug + Clone> TrackedAsyncFn<T> {
    /// Creates a new tracked async function
    ///
    /// Wraps the provided async function and sets up call tracking.
    ///
    /// # Arguments
    /// * `f` - An async function taking `(Emitter, T)` and returning `()`
    ///
    /// # Returns
    /// A `TrackedAsyncFn<T>` that automatically tracks calls
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let func = TrackedAsyncFn::new(|emitter, data| async move {
    ///     println!("Processing: {:?}", data);
    ///     // Can emit to other record types
    ///     emitter.emit(OtherData(data.value)).await?;
    /// });
    /// ```
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let stats = Arc::new(CallStats::<T>::new());
        let stats_clone = stats.clone();

        // Wrap the function in Arc so we can move it into the closure
        let f = Arc::new(f);

        // Wrap the function to record calls
        let boxed: ErasedAsyncFn<T> = Arc::new(move |em, arg| {
            let stats_inner = stats_clone.clone();
            let f = f.clone();
            Box::pin(async move {
                // Record the call
                stats_inner.record(&arg);

                // Execute the actual function
                f(em, arg).await
            })
        });

        Self { func: boxed, stats }
    }

    /// Calls the wrapped async function
    ///
    /// This will automatically record the call in statistics before
    /// executing the function.
    ///
    /// # Arguments
    /// * `emitter` - The emitter context
    /// * `arg` - The argument value
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// func.call(emitter.clone(), SensorData { temp: 23.5 }).await;
    /// ```
    pub async fn call(&self, emitter: Emitter, arg: T) {
        (self.func)(emitter, arg).await
    }

    /// Returns the call statistics
    ///
    /// # Returns
    /// An `Arc<CallStats<T>>` for inspecting call history
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let stats = func.stats();
    /// println!("Called {} times", stats.calls());
    /// println!("Last value: {:?}", stats.last_arg());
    /// ```
    pub fn stats(&self) -> Arc<CallStats<T>> {
        self.stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate alloc;
    use alloc::collections::BTreeMap;

    #[allow(dead_code)]
    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    #[allow(dead_code)]
    fn create_test_emitter() -> Emitter {
        #[cfg(not(feature = "std"))]
        use alloc::sync::Arc;
        #[cfg(feature = "std")]
        use std::sync::Arc;

        use crate::builder::AimDbInner;

        let records = BTreeMap::new();

        let inner = Arc::new(AimDbInner {
            records,
            #[cfg(feature = "std")]
            outboxes: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            #[cfg(not(feature = "std"))]
            outboxes: Arc::new(spin::Mutex::new(BTreeMap::new())),
        });
        let runtime = Arc::new(());

        Emitter::new(runtime, inner)
    }

    #[tokio::test]
    #[cfg(all(test, feature = "std"))]
    async fn test_tracked_fn_basic() {
        let func = TrackedAsyncFn::new(|_em, data: TestData| async move {
            assert_eq!(data.value, 42);
        });

        assert_eq!(func.stats().calls(), 0);

        let emitter = create_test_emitter();
        func.call(emitter, TestData { value: 42 }).await;

        assert_eq!(func.stats().calls(), 1);
        assert_eq!(func.stats().last_arg().unwrap().value, 42);
    }

    #[tokio::test]
    #[cfg(all(test, feature = "std"))]
    async fn test_tracked_fn_multiple_calls() {
        let func = TrackedAsyncFn::new(|_em, _data: TestData| async move {
            // Do nothing
        });

        let emitter = create_test_emitter();

        func.call(emitter.clone(), TestData { value: 1 }).await;
        func.call(emitter.clone(), TestData { value: 2 }).await;
        func.call(emitter, TestData { value: 3 }).await;

        assert_eq!(func.stats().calls(), 3);
        assert_eq!(func.stats().last_arg().unwrap().value, 3);
    }
}
