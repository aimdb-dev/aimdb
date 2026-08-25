//! Synchronous consumer for typed records.

use aimdb_core::{DbError, Reader};

use crate::waiter::Waiter;
use crate::{SyncError, SyncResult};
use core::fmt::Debug;
use core::time::Duration;
use std::time::Instant;

/// Synchronous consumer for records of type `T`.
///
/// A consumer is a **subscription with its own cursor**. Reading advances that
/// cursor, which is why the methods take `&mut self`. `Send` but not `Sync`,
/// and not `Clone`.
///
/// [`AimDbHandle::consumer`](crate::AimDbHandle::consumer) takes `&self` and
/// hands out as many as you like, each with an independent cursor — so the
/// default shape is **one per thread, and every consumer sees every value**.
///
/// To *split* a stream instead, so each value goes to exactly one of several
/// workers, share one consumer as `Arc<Mutex<SyncConsumer<T>>>`.
///
/// Blocking reads drive the future on the *calling* thread, so they cost no
/// runtime worker — but for the same reason they must not be called from
/// inside a Tokio runtime.
///
/// # Example
///
/// ```no_run
/// # use aimdb_sync::*;
/// # use serde::{Serialize, Deserialize};
/// # #[derive(Debug, Clone, Serialize, Deserialize)]
/// # struct Temperature { celsius: f32 }
/// # fn example(consumer: &mut SyncConsumer<Temperature>) -> SyncResult<()> {
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
    T: Send + Debug + Clone,
{
    waiter: Waiter,
    reader: Reader<T>,
}

impl<T> SyncConsumer<T>
where
    T: Send + Debug + Clone,
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
    /// - `SyncError::Db(DbError::BufferLagged)` if this consumer fell behind and
    ///   values were dropped. Not fatal, the next call resumes.
    /// - `SyncError::Db` for other errors that occurred during the read
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
    /// let mut consumer = handle.consumer::<MyData>("my_data")?;
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
    /// - `SyncError::Db(DbError::BufferLagged)` if this consumer fell behind and
    ///   values were dropped. Not fatal, the next call resumes.
    /// - `SyncError::Db` for other errors that occurred during the read
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
    /// let mut consumer = handle.consumer::<MyData>("my_data")?;
    /// match consumer.get_with_timeout(Duration::from_millis(100)) {
    ///     Ok(data) => println!("Got: {:?}", data),
    ///     Err(_) => println!("No data available"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_with_timeout(&mut self, timeout: Duration) -> SyncResult<T> {
        let fut = async { tokio::time::timeout(timeout, Self::get_impl(&mut self.reader)).await };
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
    /// - `SyncError::Db(DbError::BufferLagged)` if this consumer fell behind and
    ///   values were dropped. Not fatal, the next call resumes.
    /// - `SyncError::Db` for other errors that occurred during the read
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
    /// let mut consumer = handle.consumer::<MyData>("my_data")?;
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
    /// This method drains the buffer to get the most recent value,
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
    /// Note that the error is only reported if no value was retrieved at all.
    /// Errors occuring after that are ignored; the latest obtained value is returned instead.
    /// - `SyncError::RuntimeShutdown` if the runtime thread has stopped
    /// - `SyncError::Db` if another error occured upon the very first read.
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
    /// let mut consumer = handle.consumer::<MyData>("my_data")?;
    ///
    /// // Get the latest value, skipping any queued intermediate values
    /// let latest = consumer.get_latest()?;
    /// println!("Latest: {:?}", latest);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_latest(&mut self) -> SyncResult<T> {
        // 1) can simply sequence get_catch_up and try_get -
        //    no one else does it simultaneously thanks to &mut self
        // 2) if draining ends up with an error, we follow the previous impl
        //    and return the latest succesfully read value
        // 3) potentially loops forever if producer keeps producing
        let oldest = self.get_catch_up(None)?;
        let latest = self.drain_remaining(oldest);
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
    /// let mut consumer = handle.consumer::<MyData>("my_data")?;
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
        let deadline = Instant::now() + timeout;
        let oldest = self.get_catch_up(Some(deadline))?;
        let latest = self.drain_remaining(oldest);
        Ok(latest)
    }

    // Blocks until get() retrieves a value, or until the deadline is missed.
    // Skips BufferLagged occuring in the process, and raises all other errors
    fn get_catch_up(&mut self, deadline: Option<Instant>) -> SyncResult<T> {
        loop {
            let res = match deadline {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(Instant::now());
                    // From tokio docs: "the future is polled before the timeout is checked".
                    // We rather check if the last poll can be skipped
                    if timeout.is_zero() {
                        return Err(SyncError::GetTimeout);
                    }
                    // timeout error is handled as all other non-lag errors below
                    self.get_with_timeout(timeout)
                }
                None => self.get(),
            };
            if let Err(SyncError::Db(DbError::BufferLagged { .. })) = res {
                continue;
            } else {
                return res;
            }
        }
    }

    fn drain_remaining(&mut self, mut cur: T) -> T {
        loop {
            match self.try_get() {
                Ok(next) => cur = next,
                Err(SyncError::Db(DbError::BufferLagged { .. })) => continue,
                // errors occured during draining will be ignored
                Err(_) => return cur,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn assert_send<T: Send>() {}
    #[allow(dead_code)]
    fn check<X: Send + std::fmt::Debug + Clone>() {
        assert_send::<crate::SyncConsumer<X>>();
    }
}
