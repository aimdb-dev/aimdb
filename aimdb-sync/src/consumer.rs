//! Synchronous consumer for typed records.

use aimdb_core::DbResult;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;

/// Synchronous consumer for records of type `T`.
///
/// Thread-safe, can be cloned and shared across threads.
/// Each clone receives data independently according to buffer semantics (SPMC, etc.).
///
/// # Thread Safety
///
/// Multiple clones of `SyncConsumer<T>` can be used concurrently from
/// different threads. Each receives data independently based on the
/// configured buffer type (SPMC, SingleLatest, etc.).
///
/// # Example
///
/// ```rust,ignore
/// # use aimdb_sync::*;
/// # use serde::{Serialize, Deserialize};
/// # #[derive(Debug, Clone, Serialize, Deserialize)]
/// # struct Temperature { celsius: f32 }
/// # fn example(consumer: &SyncConsumer<Temperature>) -> Result<(), Box<dyn std::error::Error>> {
/// // Get value (blocks until available)
/// let temp = consumer.get()?;
/// println!("Temperature: {}°C", temp.celsius);
///
/// // Get with timeout
/// use std::time::Duration;
/// match consumer.get_timeout(Duration::from_millis(100)) {
///     Ok(temp) => println!("Got: {}°C", temp.celsius),
///     Err(_) => println!("No data available"),
/// }
///
/// // Try to get (non-blocking)
/// match consumer.try_get() {
///     Ok(temp) => println!("Got: {}°C", temp.celsius),
///     Err(_) => println!("No data yet"),
/// }
/// # Ok(())
/// # }
/// ```
pub struct SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Channel receiver for consumer data
    /// Wrapped in Arc so it can be cloned
    _phantom: PhantomData<T>,
}

impl<T> SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Get a value, blocking until one is available.
    ///
    /// Blocks indefinitely until a value is available from the
    /// runtime thread.
    ///
    /// # Returns
    ///
    /// The next available record of type `T`.
    ///
    /// # Errors
    ///
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ConsumeFailed` if the async consume operation fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(consumer: &SyncConsumer<Temperature>) -> Result<(), Box<dyn std::error::Error>> {
    /// let temp = consumer.get()?;
    /// println!("Temperature: {}°C", temp.celsius);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self) -> DbResult<T> {
        // TODO: Implement blocking recv
        unimplemented!("SyncConsumer::get not yet implemented")
    }

    /// Get a value with a timeout.
    ///
    /// Blocks until a value is available or the timeout expires.
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum time to wait
    ///
    /// # Errors
    ///
    /// - `DbError::GetTimeout` if the timeout expires
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ConsumeFailed` if the async consume operation fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_sync::*;
    /// # use aimdb_core::DbError;
    /// # use serde::{Serialize, Deserialize};
    /// # use std::time::Duration;
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(consumer: &SyncConsumer<Temperature>) -> Result<(), Box<dyn std::error::Error>> {
    /// match consumer.get_timeout(Duration::from_millis(100)) {
    ///     Ok(temp) => println!("Temperature: {}°C", temp.celsius),
    ///     Err(DbError::GetTimeout{ .. }) => println!("No data available"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_timeout(&self, _timeout: Duration) -> DbResult<T> {
        // TODO: Implement timeout recv
        unimplemented!("SyncConsumer::get_timeout not yet implemented")
    }

    /// Try to get a value without blocking.
    ///
    /// Returns immediately with either a value or an error if
    /// no data is available.
    ///
    /// # Errors
    ///
    /// - `DbError::GetTimeout` if no data is available (non-blocking)
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_sync::*;
    /// # use aimdb_core::DbError;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(consumer: &SyncConsumer<Temperature>) -> Result<(), Box<dyn std::error::Error>> {
    /// match consumer.try_get() {
    ///     Ok(temp) => println!("Temperature: {}°C", temp.celsius),
    ///     Err(DbError::GetTimeout{ .. }) => println!("No data yet"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_get(&self) -> DbResult<T> {
        // TODO: Implement non-blocking recv
        unimplemented!("SyncConsumer::try_get not yet implemented")
    }
}

impl<T> Clone for SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Clone the consumer to share across threads.
    ///
    /// Multiple clones can get values concurrently, each receiving
    /// independent data according to buffer semantics (SPMC, etc.).
    fn clone(&self) -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

// Safety: SyncConsumer uses Arc internally and is safe to send/share
unsafe impl<T> Send for SyncConsumer<T> where T: Send + Sync + 'static + Debug + Clone {}
unsafe impl<T> Sync for SyncConsumer<T> where T: Send + Sync + 'static + Debug + Clone {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_sync_consumer_is_send_sync() {
        // Just checking that the type implements Send + Sync
        // Actual functionality tests will come later
    }
}
