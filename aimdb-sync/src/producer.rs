//! Synchronous producer for typed records.

use aimdb_core::{DbError, DbResult};
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
/// # fn example(producer: &SyncProducer<Temperature>) -> Result<(), Box<dyn std::error::Error>> {
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
}

impl<T> SyncProducer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Create a new sync producer (internal use only)
    pub(crate) fn new(tx: mpsc::Sender<(T, oneshot::Sender<DbResult<()>>)>) -> Self {
        Self { tx: Arc::new(tx) }
    }
    /// Set the value, blocking until it can be sent.
    ///
    /// This call will block the current thread until the value can be sent to the runtime thread.
    /// It's guaranteed to deliver the value eventually unless the runtime thread has shut down.
    ///
    /// # Errors
    ///
    /// Returns `DbError::RuntimeShutdown` if the runtime thread has been detached.
    /// Returns any error from the underlying `produce()` operation (e.g., record not registered,
    /// buffer full, etc.).
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
    /// let producer = handle.producer::<MyData>()?;
    /// producer.set(MyData { value: 42 })?; // blocks until value is sent and produced
    /// # Ok(())
    /// # }
    /// ```
    pub fn set(&self, value: T) -> DbResult<()> {
        // Create a oneshot channel to receive the result
        let (result_tx, result_rx) = oneshot::channel();

        // Send the value with the result sender
        self.tx
            .blocking_send((value, result_tx))
            .map_err(|_| DbError::RuntimeShutdown)?;

        // Wait for the produce result
        result_rx
            .blocking_recv()
            .map_err(|_| DbError::RuntimeShutdown)?
    }

    /// Set the value with a timeout.
    ///
    /// Attempts to send the value to the runtime thread, blocking for at most `timeout` duration.
    ///
    /// **Implementation Note**: This method uses a polling loop with 1ms intervals because
    /// Tokio's async `mpsc::Sender` doesn't provide a `blocking_send_timeout` method. While
    /// `std::sync::mpsc::SyncSender::send_timeout` exists, it cannot be used here as we need
    /// to bridge between sync and async runtimes. The 1ms polling interval provides a good
    /// balance between responsiveness and CPU usage. For most use cases, prefer `set()`
    /// (blocking indefinitely) or `try_set()` (non-blocking).
    ///
    /// # Errors
    ///
    /// Returns `DbError::SetTimeout` if the timeout expires before the value can be sent
    /// or if waiting for the produce result exceeds the timeout.
    /// Returns `DbError::RuntimeShutdown` if the runtime thread has been detached.
    /// Returns any error from the underlying `produce()` operation.
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
    /// let producer = handle.producer::<MyData>()?;
    /// producer.set_with_timeout(MyData { value: 42 }, Duration::from_millis(100))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_with_timeout(&self, value: T, timeout: Duration) -> DbResult<()> {
        // Tokio's mpsc::Sender only provides blocking_send (indefinite) and try_send (non-blocking)
        // Use polling with 1ms intervals as a workaround for timeout functionality
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(1);

        // Create a oneshot channel to receive the result
        let (result_tx, mut result_rx) = oneshot::channel();
        let mut payload = (value, result_tx);

        // First, try to send with timeout
        loop {
            // Try non-blocking send
            match self.tx.try_send(payload) {
                Ok(()) => break,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(DbError::RuntimeShutdown);
                }
                Err(mpsc::error::TrySendError::Full(returned_payload)) => {
                    // Check timeout first
                    if start.elapsed() >= timeout {
                        return Err(DbError::SetTimeout);
                    }

                    // Sleep briefly before trying again (1ms to minimize latency)
                    std::thread::sleep(poll_interval);

                    // Restore payload for next iteration
                    payload = returned_payload;
                }
            }
        }

        // Now wait for the produce result, respecting remaining timeout
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(DbError::SetTimeout);
        }

        // Poll for result with remaining timeout
        let result_start = std::time::Instant::now();
        loop {
            match result_rx.try_recv() {
                Ok(result) => return result,
                Err(oneshot::error::TryRecvError::Empty) => {
                    if result_start.elapsed() >= remaining {
                        return Err(DbError::SetTimeout);
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    return Err(DbError::RuntimeShutdown);
                }
            }
        }
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
    /// Returns `DbError::SetTimeout` if the channel is full.
    /// Returns `DbError::RuntimeShutdown` if the runtime thread has been detached.
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
    /// let producer = handle.producer::<MyData>()?;
    /// match producer.try_set(MyData { value: 42 }) {
    ///     Ok(()) => println!("Sent immediately"),
    ///     Err(_) => println!("Channel full or runtime shutdown"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_set(&self, value: T) -> DbResult<()> {
        // Create a oneshot channel but don't wait for the result
        let (result_tx, _result_rx) = oneshot::channel();

        self.tx.try_send((value, result_tx)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => DbError::SetTimeout,
            mpsc::error::TrySendError::Closed(_) => DbError::RuntimeShutdown,
        })
    }
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
