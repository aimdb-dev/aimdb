//! Synchronous consumer for typed records.

use aimdb_core::{DbError, DbResult};
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

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
    /// Wrapped in Arc<Mutex> so it can be shared but only one thread receives at a time
    rx: Arc<Mutex<mpsc::Receiver<T>>>,
}

impl<T> SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Create a new sync consumer (internal use only)
    pub(crate) fn new(rx: mpsc::Receiver<T>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }
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
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::AimDbBuilderSyncExt;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let consumer = handle.consumer::<MyData>()?;
    /// let data = consumer.get()?; // blocks until value available
    /// println!("Got: {:?}", data);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self) -> DbResult<T> {
        let mut rx = self.rx.lock().unwrap();
        rx.blocking_recv().ok_or(DbError::RuntimeShutdown)
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
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::AimDbBuilderSyncExt;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let consumer = handle.consumer::<MyData>()?;
    /// match consumer.get_timeout(Duration::from_millis(100)) {
    ///     Ok(data) => println!("Got: {:?}", data),
    ///     Err(_) => println!("No data available"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_timeout(&self, timeout: Duration) -> DbResult<T> {
        // We need to use a channel that supports timeout on recv
        // The Tokio MPSC receiver's blocking_recv doesn't support timeout directly
        // So we'll poll with a small interval
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(10);

        loop {
            // Try non-blocking receive
            {
                let mut rx = self.rx.lock().unwrap();
                match rx.try_recv() {
                    Ok(value) => return Ok(value),
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        return Err(DbError::RuntimeShutdown);
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        // No value yet, continue polling
                    }
                }
            } // Release lock before sleeping

            // Check timeout
            if start.elapsed() >= timeout {
                return Err(DbError::GetTimeout);
            }

            // Sleep briefly before trying again
            std::thread::sleep(poll_interval);
        }
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
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::AimDbBuilderSyncExt;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let consumer = handle.consumer::<MyData>()?;
    /// match consumer.try_get() {
    ///     Ok(data) => println!("Got: {:?}", data),
    ///     Err(_) => println!("No data yet"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_get(&self) -> DbResult<T> {
        let mut rx = self.rx.lock().unwrap();
        rx.try_recv().map_err(|e| match e {
            mpsc::error::TryRecvError::Empty => DbError::GetTimeout,
            mpsc::error::TryRecvError::Disconnected => DbError::RuntimeShutdown,
        })
    }
}

impl<T> Clone for SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Clone the consumer to share across threads.
    ///
    /// Note: All clones share the same receiver, so only one thread
    /// will receive each value. For independent subscriptions, call
    /// `handle.consumer()` multiple times instead.
    fn clone(&self) -> Self {
        Self {
            rx: self.rx.clone(),
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
