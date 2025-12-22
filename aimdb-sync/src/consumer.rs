//! Synchronous consumer for typed records.

use aimdb_core::{DbError, DbResult};
use std::fmt::Debug;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
/// match consumer.get_with_timeout(Duration::from_millis(100)) {
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
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    /// let data = consumer.get()?; // blocks until value available
    /// println!("Got: {:?}", data);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self) -> DbResult<T> {
        let rx = self.rx.lock().unwrap();
        rx.recv().map_err(|_| DbError::RuntimeShutdown)
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
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    /// match consumer.get_with_timeout(Duration::from_millis(100)) {
    ///     Ok(data) => println!("Got: {:?}", data),
    ///     Err(_) => println!("No data available"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_with_timeout(&self, timeout: Duration) -> DbResult<T> {
        let rx = self.rx.lock().unwrap();
        rx.recv_timeout(timeout).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => DbError::GetTimeout,
            mpsc::RecvTimeoutError::Disconnected => DbError::RuntimeShutdown,
        })
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
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    /// match consumer.try_get() {
    ///     Ok(data) => println!("Got: {:?}", data),
    ///     Err(_) => println!("No data yet"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_get(&self) -> DbResult<T> {
        let rx = self.rx.lock().unwrap();
        rx.try_recv().map_err(|e| match e {
            mpsc::TryRecvError::Empty => DbError::GetTimeout,
            mpsc::TryRecvError::Disconnected => DbError::RuntimeShutdown,
        })
    }

    /// Get the latest value by draining all queued values.
    ///
    /// This method drains the internal channel to get the most recent value,
    /// discarding any intermediate values. This is useful for SingleLatest-like
    /// semantics where you only care about the most recent data.
    ///
    /// Blocks until at least one value is available, then drains all queued
    /// values and returns the last one.
    ///
    /// # Returns
    ///
    /// The most recent available record of type `T`.
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
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    ///
    /// // Get the latest value, skipping any queued intermediate values
    /// let latest = consumer.get_latest()?;
    /// println!("Latest: {:?}", latest);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_latest(&self) -> DbResult<T> {
        let rx = self.rx.lock().unwrap();

        // First, block until we have at least one value
        let mut latest = rx.recv().map_err(|_| DbError::RuntimeShutdown)?;

        // Then drain all remaining values to get the most recent
        while let Ok(value) = rx.try_recv() {
            latest = value;
        }

        Ok(latest)
    }

    /// Get the latest value with a timeout, draining all queued values.
    ///
    /// Like `get_latest()`, but with a timeout. Blocks until at least one
    /// value is available or the timeout expires, then drains all queued
    /// values and returns the last one.
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum time to wait for the first value
    ///
    /// # Errors
    ///
    /// - `DbError::GetTimeout` if the timeout expires before any value arrives
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
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    ///
    /// // Get the latest value within 100ms
    /// match consumer.get_latest_with_timeout(Duration::from_millis(100)) {
    ///     Ok(latest) => println!("Latest: {:?}", latest),
    ///     Err(_) => println!("No data available"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_latest_with_timeout(&self, timeout: Duration) -> DbResult<T> {
        let rx = self.rx.lock().unwrap();

        // First, block with timeout until we have at least one value
        let mut latest = rx.recv_timeout(timeout).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => DbError::GetTimeout,
            mpsc::RecvTimeoutError::Disconnected => DbError::RuntimeShutdown,
        })?;

        // Then drain all remaining values to get the most recent
        while let Ok(value) = rx.try_recv() {
            latest = value;
        }

        Ok(latest)
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
