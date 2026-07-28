//! Synchronous consumer for typed records.

use aimdb_core::buffer::BufferReader;
use aimdb_core::{DbError, Reader};

use crate::waiter::{BlockingBridge, Waiter};
use crate::{SyncError, SyncResult};
use alloc::sync::Arc;
use core::fmt::Debug;
use core::time::Duration;
use std::sync::mpsc;
use std::sync::Mutex;

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
/// ```no_run
/// # use aimdb_sync::*;
/// # use serde::{Serialize, Deserialize};
/// # #[derive(Debug, Clone, Serialize, Deserialize)]
/// # struct Temperature { celsius: f32 }
/// # fn example(consumer: &SyncConsumer<Temperature>) -> SyncResult<()> {
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
    waiter: Waiter,
    reader: Reader<T>,
}

impl<T> SyncConsumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Create a new sync consumer (internal use only)
    pub(crate) fn new(waiter: Waiter, reader: Reader<T>) -> Self {
        Self { waiter, reader }
    }

    async fn get_impl(reader: &mut Reader<T>) -> SyncResult<T> {
        let res = reader.recv().await;
        res.map_err(|e| match e {
            DbError::BufferClosed { .. } => SyncError::RuntimeShutdown,
            e => SyncError::Db(e),
        })
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
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> SyncResult<()> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let consumer = handle.consumer::<MyData>("my_data")?;
    /// let data = consumer.get()?; // blocks until value available
    /// println!("Got: {:?}", data);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&mut self) -> SyncResult<T> {
        self.waiter.block_on(Self::get_impl(&mut self.reader))
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
    /// - `SyncError::GetTimeout` if the timeout expires
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> SyncResult<()> {
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
    pub fn get_with_timeout(&mut self, timeout: Duration) -> SyncResult<T> {
        let fut = tokio::time::timeout(timeout, Self::get_impl(&mut self.reader));
        let res = self.waiter.block_on(fut);
        res.unwrap_or_else(|_| Err(SyncError::GetTimeout))
    }

    /// Try to get a value without blocking.
    ///
    /// Returns immediately with either a value or an error if
    /// no data is available.
    ///
    /// # Errors
    ///
    /// - `SyncError::GetTimeout` if no data is available (non-blocking)
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> SyncResult<()> {
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
    pub fn try_get(&mut self) -> SyncResult<T> {
        let res = self.reader.try_recv();
        res.map_err(|e| match e {
            DbError::BufferClosed { .. } => SyncError::RuntimeShutdown,
            DbError::BufferEmpty => SyncError::GetTimeout,
            e => SyncError::Db(e),
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
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> SyncResult<()> {
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
    pub fn get_latest(&mut self) -> SyncResult<T> {
        // 1) can simply sequence get and try_get -
        //    no one else does it simultaneously thanks to &mut self
        // 2) if draining ends up with an error, we follow the previous impl
        //    and return the latest succesfully read value
        // 3) potentially loops forever if producer keeps producing
        let mut latest = self.get()?;
        while let Ok(upd) = self.try_get() {
            latest = upd;
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
    /// - `SyncError::GetTimeout` if the timeout expires before any value arrives
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// # #[derive(Debug, Clone)]
    /// # struct MyData { value: i32 }
    /// # fn main() -> SyncResult<()> {
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
    pub fn get_latest_with_timeout(&mut self, timeout: Duration) -> SyncResult<T> {
        // see internal comments for get_latest
        let mut latest = self.get_with_timeout(timeout)?;
        while let Ok(upd) = self.try_get() {
            latest = upd;
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
