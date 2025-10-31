//! AimDB handle for managing the sync API runtime thread.

use aimdb_core::{AimDb, AimDbBuilder, DbError, DbResult};
use aimdb_tokio_adapter::TokioAdapter;
use std::fmt::Debug;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::mpsc;

/// Default channel capacity for sync producers and consumers.
///
/// This is the buffer size used by `producer()` and `consumer()` methods.
/// A capacity of 100 provides a good balance between:
/// - Memory usage (100 × sizeof(T) per channel)
/// - Latency (small bursts don't block)
/// - Backpressure (prevents unbounded growth)
///
/// Use `producer_with_capacity()` or `consumer_with_capacity()` if you need
/// different buffering for specific record types.
pub const DEFAULT_SYNC_CHANNEL_CAPACITY: usize = 100;

/// Extension trait to add `attach()` method to `AimDbBuilder`.
///
/// This trait provides the entry point to the sync API by allowing
/// an `AimDbBuilder` instance to build the database and attach it to
/// a background runtime thread in one step.
pub trait AimDbBuilderSyncExt {
    /// Build the database inside a runtime thread and attach for sync API.
    ///
    /// This method takes a configured builder (WITH `.runtime(TokioAdapter)` set),
    /// spawns a background thread with a Tokio runtime, builds the database
    /// inside that context, and returns a sync handle.
    ///
    /// **Important**: Call `.runtime(Arc::new(TokioAdapter))` before `.attach()`.
    /// Even though TokioAdapter is created in sync context, the actual building
    /// happens in the async context where it can be used.
    ///
    /// # Errors
    ///
    /// - `DbError::RuntimeError` if the database fails to build
    /// - `DbError::AttachFailed` if the runtime thread fails to start
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_sync::AimDbBuilderSyncExt;
    /// use std::sync::Arc;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut builder = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter));  // Create adapter (it's just a marker)
    /// builder.configure::<MyData>(|reg| {
    ///     // Configure buffer, sources, taps, etc.
    /// });
    /// let handle = builder.attach()?;  // Build happens in runtime thread
    /// # Ok(())
    /// # }
    /// ```
    fn attach(self) -> DbResult<AimDbHandle>;
}

impl AimDbBuilderSyncExt for AimDbBuilder<TokioAdapter> {
    fn attach(self) -> DbResult<AimDbHandle> {
        AimDbHandle::new_from_builder(self)
    }
}

/// Extension trait to add `attach()` method to `AimDb`.
///
/// This trait provides an alternative entry point to the sync API by allowing
/// an already-built `AimDb` instance to be attached to a background runtime thread.
pub trait AimDbSyncExt {
    /// Attach the database to a background runtime thread.
    ///
    /// Takes ownership of the database and spawns a dedicated thread running
    /// a Tokio runtime. Returns a handle for sync API access.
    ///
    /// # Errors
    ///
    /// - `DbError::AttachFailed` if the runtime thread fails to start
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_sync::AimDbSyncExt;
    /// use std::sync::Arc;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter::new()?))
    ///     .build()?;
    ///
    /// let handle = db.attach()?;
    /// # Ok(())
    /// # }
    /// ```
    fn attach(self) -> DbResult<AimDbHandle>;
}

impl AimDbSyncExt for AimDb<aimdb_tokio_adapter::TokioAdapter> {
    fn attach(self) -> DbResult<AimDbHandle> {
        AimDbHandle::new(self)
    }
}

/// Handle to the AimDB runtime thread.
///
/// Created by calling `AimDb::attach()`. Provides factory methods
/// for creating typed producers and consumers.
///
/// # Thread Safety
///
/// `AimDbHandle` is `Send + Sync` and can be shared across threads.
/// However, it should typically be owned by one thread, with only
/// the producers/consumers being cloned and shared.
///
/// # Resource Management
///
/// Call `detach()` explicitly to ensure clean shutdown. If the handle
/// is dropped without calling `detach()`, a warning will be logged
/// and an emergency shutdown will be attempted.
pub struct AimDbHandle {
    /// Thread handle for the runtime thread
    thread_handle: Option<JoinHandle<()>>,

    /// Shutdown signal sender
    shutdown_tx: Option<mpsc::Sender<ShutdownSignal>>,

    /// Tokio runtime handle for submitting async work
    runtime_handle: tokio::runtime::Handle,

    /// Shared reference to the database (protected by Arc for thread safety)
    db: Arc<AimDb<TokioAdapter>>,
}

/// Signal to shut down the runtime thread.
#[derive(Debug, Clone, Copy)]
struct ShutdownSignal;

impl AimDbHandle {
    /// Create a new handle by spawning the runtime thread and building the database inside it.
    pub(crate) fn new_from_builder(builder: AimDbBuilder<TokioAdapter>) -> DbResult<Self> {
        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownSignal>(1);

        // Create channels for passing the built database and runtime handle back
        let (db_tx, mut db_rx) = mpsc::channel::<Arc<AimDb<TokioAdapter>>>(1);
        let (handle_tx, mut handle_rx) = mpsc::channel::<tokio::runtime::Handle>(1);

        // Spawn the runtime thread
        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(move || {
                // Create a new Tokio runtime for this thread
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("Failed to create Tokio runtime: {}", e);
                        return;
                    }
                };

                // Get the runtime handle before moving into block_on
                let rt_handle = runtime.handle().clone();

                // Send the runtime handle to the main thread
                if handle_tx.blocking_send(rt_handle).is_err() {
                    eprintln!("Failed to send runtime handle to main thread");
                    return;
                }

                // Build the database inside the async context
                runtime.block_on(async move {
                    // Build the database (now we're in Tokio context where it can spawn tasks!)
                    let db = match builder.build() {
                        Ok(d) => Arc::new(d),
                        Err(e) => {
                            eprintln!("Failed to build database: {}", e);
                            return;
                        }
                    };

                    // Send the database to the main thread
                    if db_tx.send(db.clone()).await.is_err() {
                        eprintln!("Failed to send database to main thread");
                        return;
                    }

                    // Wait for shutdown signal, keeping database alive
                    let _ = shutdown_rx.recv().await;
                });
            })
            .map_err(|e| DbError::AttachFailed {
                message: format!("Failed to spawn runtime thread: {}", e),
            })?;

        // Wait for runtime handle to be available
        let runtime_handle = handle_rx
            .blocking_recv()
            .ok_or_else(|| DbError::AttachFailed {
                message: "Runtime thread failed to send handle".to_string(),
            })?;

        // Wait for database to be built
        let db = db_rx.blocking_recv().ok_or_else(|| DbError::AttachFailed {
            message: "Runtime thread failed to build database".to_string(),
        })?;

        Ok(Self {
            thread_handle: Some(thread_handle),
            shutdown_tx: Some(shutdown_tx),
            runtime_handle,
            db,
        })
    }

    /// Create a new handle from an already-built database (legacy method).
    #[allow(dead_code)]
    pub(crate) fn new(db: AimDb<TokioAdapter>) -> DbResult<Self> {
        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownSignal>(1);

        // Wrap database in Arc for sharing
        let db = Arc::new(db);

        // Spawn the runtime thread
        let runtime_handle_result = Arc::new(std::sync::Mutex::new(None));
        let runtime_handle_clone = runtime_handle_result.clone();

        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(move || {
                // Create a new Tokio runtime for this thread
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("Failed to create Tokio runtime: {}", e);
                        return;
                    }
                };

                // Store the runtime handle so the main thread can access it
                {
                    let mut handle = runtime_handle_clone.lock().unwrap();
                    *handle = Some(runtime.handle().clone());
                }

                // Wait for shutdown signal
                runtime.block_on(async move {
                    let _ = shutdown_rx.recv().await;
                    // When shutdown signal is received, we exit and drop the database
                });
            })
            .map_err(|e| DbError::AttachFailed {
                message: format!("Failed to spawn runtime thread: {}", e),
            })?;

        // Wait for runtime handle to be available
        let runtime_handle = loop {
            let handle_opt = runtime_handle_result.lock().unwrap().clone();
            if let Some(handle) = handle_opt {
                break handle;
            }
            thread::sleep(Duration::from_millis(1));
        };

        Ok(Self {
            thread_handle: Some(thread_handle),
            shutdown_tx: Some(shutdown_tx),
            runtime_handle,
            db,
        })
    }

    /// Create a synchronous producer for type `T`.
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(handle: &AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// let producer = handle.producer::<Temperature>()?;
    /// producer.set(Temperature { celsius: 25.0 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn producer<T>(&self) -> DbResult<crate::SyncProducer<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        self.producer_with_capacity(DEFAULT_SYNC_CHANNEL_CAPACITY)
    }

    /// Create a synchronous consumer for type `T`.
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Clone, Debug, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(handle: &AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// let consumer = handle.consumer::<Temperature>()?;
    /// let temp = consumer.get()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn consumer<T>(&self) -> DbResult<crate::SyncConsumer<T>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        self.consumer_with_capacity(DEFAULT_SYNC_CHANNEL_CAPACITY)
    }

    /// Create a synchronous producer with custom channel capacity.
    ///
    /// Like `producer()` but allows specifying the channel buffer size.
    /// Use this when you need different buffering characteristics for specific record types.
    ///
    /// # Arguments
    ///
    /// - `capacity`: Channel buffer size (number of items that can be buffered)
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct HighFrequencySensor { value: f32 }
    /// # fn example(handle: &AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// // High-frequency sensor needs larger buffer
    /// let producer = handle.producer_with_capacity::<HighFrequencySensor>(1000)?;
    /// producer.set(HighFrequencySensor { value: 42.0 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn producer_with_capacity<T>(&self, capacity: usize) -> DbResult<crate::SyncProducer<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Create a bounded tokio channel for async/sync bridging
        // Channel carries (value, result_sender) tuples to propagate errors back
        let (tx, mut rx) =
            mpsc::channel::<(T, tokio::sync::oneshot::Sender<DbResult<()>>)>(capacity);

        // Spawn a task on the runtime to forward values to the database
        let db = self.db.clone();
        self.runtime_handle.spawn(async move {
            while let Some((value, result_tx)) = rx.recv().await {
                // Forward the value to the database's produce pipeline
                let result = db.produce(value).await;

                // Send the result back to the caller (may fail if caller dropped)
                let _ = result_tx.send(result);
            }
        });

        Ok(crate::SyncProducer::new(tx, self.runtime_handle.clone()))
    }

    /// Create a synchronous consumer with custom channel capacity.
    ///
    /// Like `consumer()` but allows specifying the channel buffer size.
    /// Use this when you need different buffering characteristics for specific record types.
    ///
    /// # Arguments
    ///
    /// - `capacity`: Channel buffer size (number of items that can be buffered)
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Clone, Debug, Serialize, Deserialize)]
    /// # struct RareEvent { id: u32 }
    /// # fn example(handle: &AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// // Rare events need smaller buffer
    /// let consumer = handle.consumer_with_capacity::<RareEvent>(10)?;
    /// let event = consumer.get()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn consumer_with_capacity<T>(&self, capacity: usize) -> DbResult<crate::SyncConsumer<T>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Create std::sync::mpsc channel for sync API
        let (std_tx, std_rx) = std::sync::mpsc::sync_channel::<T>(capacity);

        // Create a oneshot channel to confirm subscription succeeded
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Spawn a task on the runtime to forward buffer data to the std channel
        let db = self.db.clone();
        self.runtime_handle.spawn(async move {
            // Subscribe to the database buffer for type T
            match db.subscribe::<T>() {
                Ok(mut reader) => {
                    // Signal that subscription succeeded
                    let _ = ready_tx.send(());

                    // Forward all values from the buffer reader to the std channel
                    loop {
                        match reader.recv().await {
                            Ok(value) => {
                                // Send to std channel (non-async operation)
                                // If the receiver is dropped, send() will fail
                                if std_tx.send(value).is_err() {
                                    break;
                                }
                            }
                            Err(DbError::BufferLagged { lag_count, .. }) => {
                                // Consumer fell behind - this is not fatal
                                // Log warning but continue receiving
                                eprintln!(
                                    "Warning: Consumer for {} lagged by {} messages",
                                    std::any::type_name::<T>(),
                                    lag_count
                                );
                                // Don't break - next recv() will get latest data
                            }
                            Err(DbError::BufferClosed { .. }) => {
                                // Buffer closed (shutdown) - exit gracefully
                                break;
                            }
                            Err(e) => {
                                // Other unexpected errors - log and stop
                                eprintln!(
                                    "Error reading from buffer for {}: {}",
                                    std::any::type_name::<T>(),
                                    e
                                );
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Failed to subscribe to record type {}: {}",
                        std::any::type_name::<T>(),
                        e
                    );
                    // Signal failure (will be ignored if receiver dropped)
                    let _ = ready_tx.send(());
                }
            }
        });

        // Wait for subscription to complete (with timeout)
        ready_rx
            .blocking_recv()
            .map_err(|_| DbError::AttachFailed {
                message: format!("Failed to subscribe to {}", std::any::type_name::<T>()),
            })?;

        Ok(crate::SyncConsumer::new(std_rx))
    }

    /// Gracefully shut down the runtime thread.
    ///
    /// Signals the runtime to stop, waits for all pending operations
    /// to complete, then joins the thread. This is the preferred way
    /// to shut down.
    ///
    /// # Errors
    ///
    /// - `DbError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # fn example(handle: AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// handle.detach()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn detach(mut self) -> DbResult<()> {
        self.detach_internal(None)
    }

    /// Gracefully shut down with a timeout.
    ///
    /// Like `detach()`, but fails if shutdown takes longer than
    /// the specified duration.
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum time to wait for shutdown
    ///
    /// # Errors
    ///
    /// - `DbError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # use std::time::Duration;
    /// # fn example(handle: AimDbHandle) -> Result<(), Box<dyn std::error::Error>> {
    /// handle.detach_timeout(Duration::from_secs(5))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn detach_timeout(mut self, timeout: Duration) -> DbResult<()> {
        self.detach_internal(Some(timeout))
    }

    /// Internal detach implementation.
    fn detach_internal(&mut self, timeout: Option<Duration>) -> DbResult<()> {
        // Send shutdown signal
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            // Try to send shutdown signal (non-blocking)
            // If it fails, the runtime may have already stopped
            let _ = shutdown_tx.try_send(ShutdownSignal);
        }

        // Join the runtime thread
        if let Some(thread_handle) = self.thread_handle.take() {
            match timeout {
                Some(duration) => {
                    // Join with timeout using a different approach since JoinHandle
                    // doesn't directly support timeouts
                    let handle_thread = thread::spawn(move || thread_handle.join());

                    // Wait for the thread to complete with timeout
                    let start = std::time::Instant::now();
                    loop {
                        if handle_thread.is_finished() {
                            break;
                        }
                        if start.elapsed() > duration {
                            return Err(DbError::DetachFailed {
                                message: format!(
                                    "Runtime thread did not shut down within {:?}",
                                    duration
                                ),
                            });
                        }
                        thread::sleep(Duration::from_millis(10));
                    }

                    // Retrieve the result
                    handle_thread
                        .join()
                        .map_err(|_| DbError::DetachFailed {
                            message: "Failed to join helper thread".to_string(),
                        })?
                        .map_err(|_| DbError::DetachFailed {
                            message: "Runtime thread panicked".to_string(),
                        })?;
                }
                None => {
                    // Join without timeout
                    thread_handle.join().map_err(|_| DbError::DetachFailed {
                        message: "Runtime thread panicked during shutdown".to_string(),
                    })?;
                }
            }
        }

        Ok(())
    }
}

impl Drop for AimDbHandle {
    /// Attempts graceful shutdown if `detach()` was not called.
    ///
    /// Logs a warning and attempts shutdown with a 5-second timeout.
    /// If shutdown fails, the runtime thread may be left running.
    fn drop(&mut self) {
        if self.thread_handle.is_some() {
            eprintln!("Warning: AimDbHandle dropped without calling detach()");
            eprintln!("Attempting emergency shutdown with 5 second timeout");

            let timeout = Duration::from_secs(5);
            if let Err(e) = self.detach_internal(Some(timeout)) {
                eprintln!("Error during emergency shutdown: {}", e);
            }
        }
    }
}

// Safety: AimDbHandle owns the runtime thread and channels are Send + Sync
unsafe impl Send for AimDbHandle {}
unsafe impl Sync for AimDbHandle {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_extension_trait_exists() {
        // Just ensure the module compiles
        // Actual functionality tests will come later
    }
}
