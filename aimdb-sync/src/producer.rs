//! Synchronous producer for typed records.

use crate::{SyncError, SyncResult};
use aimdb_core::DbResult;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Synchronous producer for records of type `T`.
///
/// Thread-safe, can be cloned and shared across threads.
/// Values are moved (not cloned) through channels for zero-copy performance.
///
/// # Thread Safety
///
/// Multiple clones of `SyncProducer<T>` can be used concurrently from
/// different threads. Each `set()` operation is independent and thread-safe.
///
/// # Example
///
/// ```rust,no_run
/// # use aimdb_sync::*;
/// # use serde::{Serialize, Deserialize};
/// # #[derive(Clone, Debug, Serialize, Deserialize)]
/// # struct Temperature { celsius: f32 }
/// # fn example(producer: &SyncProducer<Temperature>) -> SyncResult<()> {
/// // Set value (blocks until sent)
/// producer.set(Temperature { celsius: 25.0 })?;
///
/// // Set with timeout
/// use std::time::Duration;
/// producer.set_with_timeout(
///     Temperature { celsius: 26.0 },
///     Duration::from_millis(100)
/// )?;
///
/// // Try to set (non-blocking)
/// match producer.try_set(Temperature { celsius: 27.0 }) {
///     Ok(()) => println!("Success"),
///     Err(_) => println!("Channel full, try later"),
/// }
/// # Ok(())
/// # }
/// ```
pub struct SyncProducer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Channel sender for producer commands
    /// Wrapped in Arc so it can be cloned across threads
    /// Sends (value, result_sender) tuples to propagate produce errors back to caller
    tx: Arc<mpsc::Sender<(T, oneshot::Sender<DbResult<()>>)>>,

    /// Runtime handle for executing async operations with timeout
    runtime_handle: tokio::runtime::Handle,
}

impl<T> SyncProducer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Create a new sync producer (internal use only)
    pub(crate) fn new(
        tx: mpsc::Sender<(T, oneshot::Sender<DbResult<()>>)>,
        runtime_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            tx: Arc::new(tx),
            runtime_handle,
        }
    }

    /// Internal helper: send value and wait for result with optional timeout
    fn send_internal(&self, value: T, timeout: Option<Duration>) -> SyncResult<()> {
        let (result_tx, result_rx) = oneshot::channel();
        let tx = self.tx.clone();

        self.runtime_handle.block_on(async move {
            // Send with optional timeout
            let send_result = match timeout {
                Some(duration) => tokio::time::timeout(duration, tx.send((value, result_tx))).await,
                None => Ok(tx.send((value, result_tx)).await),
            };

            match send_result {
                Ok(Ok(())) => {
                    // Successfully sent, now wait for produce result
                    let recv_result = match timeout {
                        Some(duration) => tokio::time::timeout(duration, result_rx).await,
                        None => Ok(result_rx.await),
                    };

                    match recv_result {
                        Ok(Ok(result)) => result.map_err(SyncError::from),
                        Ok(Err(_)) => Err(SyncError::RuntimeShutdown),
                        Err(_) => Err(SyncError::SetTimeout),
                    }
                }
                Ok(Err(_)) => Err(SyncError::RuntimeShutdown),
                Err(_) => Err(SyncError::SetTimeout),
            }
        })
    }

    /// Set the value, blocking until it can be sent.
    ///
    /// This call will block the current thread until the value can be sent to the runtime thread.
    /// It's guaranteed to deliver the value eventually unless the runtime thread has shut down.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::RuntimeShutdown` if the runtime thread has been detached.
    /// Returns any error from the underlying `produce()` operation (e.g., record not registered,
    /// buffer full, etc.).
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
    /// # #[cfg(feature = "std")]
    /// # fn main() -> SyncResult<()> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let producer = handle.producer::<MyData>("my_data")?;
    /// producer.set(MyData { value: 42 })?; // blocks until value is sent and produced
    /// # Ok(())
    /// # }
    /// ```
    pub fn set(&self, value: T) -> SyncResult<()> {
        self.send_internal(value, None)
    }

    /// Set the value with a timeout.
    ///
    /// Attempts to send the value to the runtime thread and wait for produce completion,
    /// blocking for at most `timeout` duration.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::SetTimeout` if the timeout expires before the value can be sent
    /// or if waiting for the produce result exceeds the timeout.
    /// Returns `SyncError::RuntimeShutdown` if the runtime thread has been detached.
    /// Returns any error from the underlying `produce()` operation.
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
    /// # #[cfg(feature = "std")]
    /// # fn main() -> SyncResult<()> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let producer = handle.producer::<MyData>("my_data")?;
    /// producer.set_with_timeout(MyData { value: 42 }, Duration::from_millis(100))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_with_timeout(&self, value: T, timeout: Duration) -> SyncResult<()> {
        self.send_internal(value, Some(timeout))
    }

    /// Try to set the value without blocking.
    ///
    /// Attempts to send the value immediately. Returns an error if the channel is full
    /// or the runtime thread has shut down.
    ///
    /// **Note**: This method returns immediately after sending to the channel, but does NOT
    /// wait for the produce operation to complete. Use `set()` or `set_with_timeout()` if
    /// you need to know whether the produce operation succeeded.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::SetTimeout` if the channel is full.
    /// Returns `SyncError::RuntimeShutdown` if the runtime thread has been detached.
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
    /// # #[cfg(feature = "std")]
    /// # fn main() -> SyncResult<()> {
    /// let handle = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter))
    ///     .attach()?;
    /// let producer = handle.producer::<MyData>("my_data")?;
    /// match producer.try_set(MyData { value: 42 }) {
    ///     Ok(()) => println!("Sent immediately"),
    ///     Err(_) => println!("Channel full or runtime shutdown"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_set(&self, value: T) -> SyncResult<()> {
        // Create a oneshot channel but don't wait for the result
        let (result_tx, _result_rx) = oneshot::channel();

        self.tx.try_send((value, result_tx)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SyncError::SetTimeout,
            mpsc::error::TrySendError::Closed(_) => SyncError::RuntimeShutdown,
        })
    }
}

/// Set-by-primitive verbs for `Settable` types (feature `data-contracts`).
///
/// Where [`set`](Self::set) takes a fully constructed `T`, `set_value`
/// constructs it via `T::set(value, timestamp)` and sends, in one call.
/// Distinct from AimX's `record.set {name, value}` (full JSON value through
/// `JsonCodec`) — this is set-by-primitive.
#[cfg(feature = "data-contracts")]
impl<T> SyncProducer<T>
where
    T: aimdb_data_contracts::Settable + Send + 'static + Debug + Clone,
{
    /// Construct via `T::set(value, now)` and send. Blocking, like [`set`](Self::set).
    ///
    /// Stamps with the *caller's* `SystemTime` (sample time at the edge), not
    /// the engine's `ctx.time()` — use [`set_value_at`](Self::set_value_at) for
    /// explicit-timestamp control (replay, testing).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "data-contracts")]
    /// # #[cfg(feature = "std")]
    /// # fn main() -> SyncResult<()> {
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_data_contracts::{SchemaType, Settable};
    /// use aimdb_sync::AimDbBuilderSyncExt;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// #[derive(Debug, Clone)]
    /// struct Temperature { celsius: f32, timestamp: u64 }
    ///
    /// impl SchemaType for Temperature {
    ///     const NAME: &'static str = "temperature";
    /// }
    ///
    /// impl Settable for Temperature {
    ///     type Value = f32;
    ///     fn set(value: f32, timestamp: u64) -> Self {
    ///         Temperature { celsius: value, timestamp }
    ///     }
    /// }
    ///
    /// let handle = AimDbBuilder::new().runtime(Arc::new(TokioAdapter)).attach()?;
    /// let producer = handle.producer::<Temperature>("temperature")?;
    /// producer.set_value(22.5)?; // constructs Temperature::set(22.5, now_ms) and sends
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "data-contracts"))]
    /// # fn main() {}
    /// ```
    pub fn set_value(&self, value: T::Value) -> SyncResult<()> {
        self.set(T::set(value, unix_now_ms()))
    }

    /// Non-blocking variant, like [`try_set`](Self::try_set).
    pub fn try_set_value(&self, value: T::Value) -> SyncResult<()> {
        self.try_set(T::set(value, unix_now_ms()))
    }

    /// Explicit-timestamp variant (replay, testing).
    pub fn set_value_at(&self, value: T::Value, timestamp_ms: u64) -> SyncResult<()> {
        self.set(T::set(value, timestamp_ms))
    }
}

/// Current wall-clock time as Unix milliseconds (caller-side clock).
#[cfg(feature = "data-contracts")]
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl<T> Clone for SyncProducer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Clone the producer to share across threads.
    ///
    /// Multiple clones can set values concurrently.
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            runtime_handle: self.runtime_handle.clone(),
        }
    }
}

// Safety: SyncProducer uses Arc internally and is safe to send/share
unsafe impl<T> Send for SyncProducer<T> where T: Send + 'static + Debug + Clone {}
unsafe impl<T> Sync for SyncProducer<T> where T: Send + 'static + Debug + Clone {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_sync_producer_is_send_sync() {
        // Just checking that the type implements Send + Sync
        // Actual functionality tests will come later
    }
}
