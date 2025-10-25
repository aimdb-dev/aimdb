//! Type-safe producer API for emitting values
//!
//! Provides the `Producer<T>` type for scoped, type-safe value production.
//! This is the preferred way to pass database access to producer services,
//! following the principle of least privilege.
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_core::{Producer, RuntimeContext, service};
//!
//! #[service]
//! async fn temperature_producer<R: Runtime>(
//!     ctx: RuntimeContext<R>,
//!     producer: Producer<Temperature>,  // Type-safe, scoped access
//! ) {
//!     let log = ctx.log();
//!     loop {
//!         let temp = read_sensor().await;
//!         producer.produce(temp).await?;
//!         ctx.time().sleep(ctx.time().secs(1)).await;
//!     }
//! }
//! ```

use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;
use alloc::sync::Arc;

use crate::{AimDb, DbResult};

/// Type-safe producer for a specific record type
///
/// `Producer<T>` provides scoped access to produce values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it produces
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
///
/// # Example
///
/// ```rust,ignore
/// // Create a type-safe producer
/// let db = builder.build()?;
/// let temp_producer = db.producer::<Temperature>();
///
/// // Pass to service - it can only produce Temperature values
/// runtime.spawn(temperature_service(ctx, temp_producer)).unwrap();
/// ```
pub struct Producer<T> {
    /// Reference to the database (type-erased)
    db: Arc<AimDb>,

    /// Phantom data to bind the type parameter
    _phantom: PhantomData<T>,
}

impl<T> Producer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Create a new producer (internal use only)
    ///
    /// Use `AimDb::producer()` to create producers from application code.
    pub(crate) fn new(db: Arc<AimDb>) -> Self {
        Self {
            db,
            _phantom: PhantomData,
        }
    }

    /// Produce a value of type T
    ///
    /// This triggers the entire pipeline for this record type:
    /// 1. All tap observers are notified
    /// 2. All link connectors are triggered
    /// 3. Buffers are updated (if configured)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The record type is not registered
    /// - A connector fails to serialize/publish
    /// - The underlying runtime adapter reports an error
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let temp = Temperature {
    ///     sensor_id: "sensor-001".to_string(),
    ///     celsius: 23.5,
    ///     timestamp: now(),
    /// };
    ///
    /// producer.produce(temp).await?;
    /// ```
    pub async fn produce(&self, value: T) -> DbResult<()> {
        self.db.produce(value).await
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            _phantom: PhantomData,
        }
    }
}

// Implement Send + Sync for Producer
// Safe because:
// - AimDb is already Send + Sync (contains Arc)
// - PhantomData<T> is Send + Sync when T is Send
unsafe impl<T: Send> Send for Producer<T> {}
unsafe impl<T: Send> Sync for Producer<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    struct TestRecord {
        value: i32,
    }

    #[test]
    fn test_producer_clone() {
        // This is a compile-time test to ensure Producer<T> is Clone
        fn assert_clone<T: Clone>() {}
        assert_clone::<Producer<TestRecord>>();
    }

    #[test]
    fn test_producer_send_sync() {
        // This is a compile-time test to ensure Producer<T> is Send + Sync
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Producer<TestRecord>>();
        assert_sync::<Producer<TestRecord>>();
    }
}
