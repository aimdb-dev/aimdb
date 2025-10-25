//! Type-safe consumer API for subscribing to values
//!
//! Provides the `Consumer<T>` type for scoped, type-safe value consumption.
//! This is the preferred way to pass database access to consumer services,
//! following the principle of least privilege.
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_core::{Consumer, RuntimeContext, service};
//!
//! #[service]
//! async fn temperature_monitor<R: Runtime>(
//!     ctx: RuntimeContext<R>,
//!     consumer: Consumer<Temperature>,  // Type-safe, scoped access
//! ) {
//!     let log = ctx.log();
//!     
//!     // Subscribe to temperature updates
//!     let mut rx = consumer.subscribe().await?;
//!     
//!     while let Some(temp) = rx.recv().await {
//!         log.info(&format!("Received: {:.1}°C", temp.celsius));
//!     }
//! }
//! ```

use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;
use alloc::boxed::Box;
use alloc::sync::Arc;

use crate::{AimDb, DbResult};

/// Type-safe consumer for a specific record type
///
/// `Consumer<T>` provides scoped access to subscribe to values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it consumes
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
///
/// # Example
///
/// ```rust,ignore
/// // Create a type-safe consumer
/// let db = builder.build()?;
/// let temp_consumer = db.consumer::<Temperature>();
///
/// // Pass to service - it can only subscribe to Temperature values
/// runtime.spawn(temperature_monitor(ctx, temp_consumer)).unwrap();
/// ```
#[derive(Clone)]
pub struct Consumer<T> {
    /// Reference to the database (type-erased)
    db: Arc<AimDb>,

    /// Phantom data to bind the type parameter
    _phantom: PhantomData<T>,
}

impl<T> Consumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Create a new consumer (internal use only)
    ///
    /// Use `AimDb::consumer()` to create consumers from application code.
    pub(crate) fn new(db: Arc<AimDb>) -> Self {
        Self {
            db,
            _phantom: PhantomData,
        }
    }

    /// Subscribe to updates for this record type
    ///
    /// Returns a reader that yields values when they are produced.
    /// This is useful for services that react to data changes.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The record type is not registered
    /// - No buffer is configured for this record type
    /// - The underlying runtime adapter reports an error
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut rx = consumer.subscribe()?;
    ///
    /// loop {
    ///     match rx.recv().await {
    ///         Ok(temp) => println!("Temperature: {:.1}°C", temp.celsius),
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub fn subscribe(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>> {
        self.db.subscribe::<T>()
    }
}

// Implement Send + Sync for Consumer
// Safe because:
// - AimDb is already Send + Sync (contains Arc)
// - PhantomData<T> is Send + Sync when T is Send
unsafe impl<T: Send> Send for Consumer<T> {}
unsafe impl<T: Send> Sync for Consumer<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    struct TestRecord {
        value: i32,
    }

    #[test]
    fn test_consumer_clone() {
        // This is a compile-time test to ensure Consumer<T> is Clone
        fn assert_clone<T: Clone>() {}
        assert_clone::<Consumer<TestRecord>>();
    }

    #[test]
    fn test_consumer_send_sync() {
        // Compile-time test for Send + Sync bounds
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<Consumer<TestRecord>>();
        assert_sync::<Consumer<TestRecord>>();
    }
}
