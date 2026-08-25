//! AimDB handle for managing the sync API runtime thread.

use crate::waiter::Waiter;
use crate::{SyncError, SyncResult};
use aimdb_core::{log_error, log_warn, AimDb, AimDbBuilder};
use alloc::sync::Arc;
use core::fmt::Debug;
use core::time::Duration;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc;

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
    /// - `SyncError::AttachFailed` if the runtime thread fails to start
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDbBuilder;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
    /// use std::sync::Arc;
    ///
    /// # #[derive(Debug, Clone)] struct MyData { value: f32 }
    /// # fn main() -> SyncResult<()> {
    /// let mut builder = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter::new()?));
    /// builder.configure::<MyData>("my.data", |reg| {
    ///     // Configure buffer, sources, taps, etc.
    /// });
    /// let handle = builder.attach()?;  // Build happens in runtime thread
    /// # Ok(())
    /// # }
    /// ```
    fn attach(self) -> SyncResult<AimDbHandle>;
}

impl AimDbBuilderSyncExt for AimDbBuilder {
    fn attach(self) -> SyncResult<AimDbHandle> {
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
    /// - `SyncError::AttachFailed` if the runtime thread fails to start
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aimdb_core::AimDb;
    /// use aimdb_sync::{AimDbSyncExt, SyncResult};
    ///
    /// // `db` comes out of an async `AimDbBuilder::build()` elsewhere
    /// # fn demo(db: AimDb) -> SyncResult<()> {
    /// let handle = db.attach()?;
    /// # Ok(())
    /// # }
    /// ```
    fn attach(self) -> SyncResult<AimDbHandle>;
}

impl AimDbSyncExt for AimDb {
    fn attach(self) -> SyncResult<AimDbHandle> {
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
    db: Arc<AimDb>,

    /// The fork generation this handle was created in. A `fork` copies this
    /// struct but not `thread_handle`'s thread. See [`crate::fork`].
    made_in: crate::fork::Generation,
}

/// Signal to shut down the runtime thread.
#[derive(Debug, Clone, Copy)]
struct ShutdownSignal;

/// What the runtime thread reports back while starting up: the thing itself,
/// or why it could not be produced.
type Startup<T> = Result<T, String>;

/// Wait for one startup report, turning every outcome into a `SyncResult`.
///
/// The `None` arm is what makes this a wait rather than a hang: a runtime
/// thread that dies without reporting drops its sender, and a dropped sender
/// closes the channel.
///
/// Blocks the calling thread, so it must not be called from inside a Tokio
/// runtime.
fn recv_startup<T>(rx: &mut mpsc::Receiver<Startup<T>>, what: &str) -> SyncResult<T> {
    match rx.blocking_recv() {
        Some(Ok(value)) => Ok(value),
        Some(Err(cause)) => Err(SyncError::AttachFailed { message: cause }),
        None => Err(SyncError::AttachFailed {
            message: format!("runtime thread stopped before sending the {}", what),
        }),
    }
}

impl AimDbHandle {
    /// Create a new handle by spawning the runtime thread and building the database inside it.
    pub(crate) fn new_from_builder(builder: AimDbBuilder) -> SyncResult<Self> {
        // Lazily, so a program that never attaches never installs a handler.
        crate::fork::arm();

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownSignal>(1);

        // Create channels for passing the built database and runtime handle back
        let (db_tx, mut db_rx) = mpsc::channel::<Startup<Arc<AimDb>>>(1);
        let (handle_tx, mut handle_rx) = mpsc::channel::<Startup<tokio::runtime::Handle>>(1);

        // Spawn the runtime thread
        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(|| Self::setup_background(builder, shutdown_rx, db_tx, handle_tx))
            .map_err(|e| SyncError::AttachFailed {
                message: format!("Failed to spawn runtime thread: {}", e),
            })?;

        // Both report the thread's own reason for failing rather than only the
        // fact that it failed.
        let runtime_handle = recv_startup(&mut handle_rx, "runtime handle")?;
        let db = recv_startup(&mut db_rx, "database")?;

        Ok(Self {
            thread_handle: Some(thread_handle),
            shutdown_tx: Some(shutdown_tx),
            runtime_handle,
            db,
            made_in: crate::fork::generation(),
        })
    }

    pub(crate) fn new(db: AimDb) -> SyncResult<Self> {
        crate::fork::arm();

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownSignal>(1);

        // A channel rather than a mutex the caller polls — the same shape
        // `new_from_builder` has always used. See `recv_startup`.
        let (handle_tx, mut handle_rx) = mpsc::channel::<Startup<tokio::runtime::Handle>>(1);

        // Wrap database in Arc for sharing
        let db = Arc::new(db);

        // Spawn the runtime thread
        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(move || {
                // Create a new Tokio runtime for this thread
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        // Report the reason before dying. Dropping the sender
                        // would already unblock the caller; this is what gives
                        // it something to print.
                        let cause = format!("Failed to create Tokio runtime: {}", e);
                        log_error!("{}", cause);
                        let _ = handle_tx.blocking_send(Err(cause));
                        return;
                    }
                };

                // Hand the runtime handle to the caller.
                if handle_tx
                    .blocking_send(Ok(runtime.handle().clone()))
                    .is_err()
                {
                    log_error!("Failed to send runtime handle to main thread");
                    return;
                }

                // Wait for shutdown signal
                runtime.block_on(async move {
                    let _ = shutdown_rx.recv().await;
                    // When shutdown signal is received, we exit and drop the database
                });
            })
            .map_err(|e| SyncError::AttachFailed {
                message: format!("Failed to spawn runtime thread: {}", e),
            })?;

        let runtime_handle = recv_startup(&mut handle_rx, "runtime handle")?;

        Ok(Self {
            thread_handle: Some(thread_handle),
            shutdown_tx: Some(shutdown_tx),
            runtime_handle,
            db,
            made_in: crate::fork::generation(),
        })
    }

    /// Refuse if this process has forked since the handle was created.
    #[inline]
    fn check_fork(&self) -> SyncResult<()> {
        if crate::fork::forked_since(self.made_in) {
            return Err(SyncError::ForkedChild);
        }
        Ok(())
    }

    /// Create a synchronous producer for type `T`.
    ///
    /// # Arguments
    ///
    /// - `key`: The record key identifying this record instance
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Debug, Clone, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(handle: &AimDbHandle) -> SyncResult<()> {
    /// let producer = handle.producer::<Temperature>("sensor::temp")?;
    /// producer.set(Temperature { celsius: 25.0 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn producer<T>(&self, key: impl AsRef<str>) -> SyncResult<crate::SyncProducer<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        self.check_fork()?;
        Ok(crate::SyncProducer::new(Arc::downgrade(&self.db), key))
    }

    /// Create a synchronous consumer for type `T`.
    ///
    /// # Arguments
    ///
    /// - `key`: The record key identifying this record instance
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors (wrapped in SyncError::Db)
    ///
    /// - `DbError::RecordKeyNotFound` if type `T` was not registered
    /// - `DbError::TypeMismatch` if the record type does not match `T`
    /// - `DbError::MissingConfiguration` if the corresponding buffer was not configured
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Clone, Debug, Serialize, Deserialize)]
    /// # struct Temperature { celsius: f32 }
    /// # fn example(handle: &AimDbHandle) -> SyncResult<()> {
    /// let mut consumer = handle.consumer::<Temperature>("sensor::temp")?;
    /// let temp = consumer.get()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn consumer<T>(&self, key: impl AsRef<str>) -> SyncResult<crate::SyncConsumer<T>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        self.check_fork()?;
        let record_key = key.as_ref().to_string();
        let reader = self.db.subscribe::<T>(&record_key).map_err(SyncError::Db)?;
        let waiter = Waiter::new(self.runtime_handle.clone());
        Ok(crate::SyncConsumer::new(waiter, reader))
    }

    /// Gracefully shut down the runtime thread.
    ///
    /// Signals the runtime to stop, waits for all pending operations
    /// to complete, then joins the thread. This is the preferred way
    /// to shut down.
    ///
    /// # Errors
    ///
    /// - `SyncError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # fn example(handle: AimDbHandle) -> SyncResult<()> {
    /// handle.detach()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn detach(mut self) -> SyncResult<()> {
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
    /// - `SyncError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aimdb_sync::*;
    /// # use std::time::Duration;
    /// # fn example(handle: AimDbHandle) -> SyncResult<()> {
    /// handle.detach_timeout(Duration::from_secs(5))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn detach_timeout(mut self, timeout: Duration) -> SyncResult<()> {
        self.detach_internal(Some(timeout))
    }

    /// Internal detach implementation.
    fn detach_internal(&mut self, timeout: Option<Duration>) -> SyncResult<()> {
        // A forked child holds a `JoinHandle` for a thread that does not exist
        // here, and joining it is not merely useless: it panics inside `std`
        // with "threads should not terminate unexpectedly", which for an FFI
        // caller means a Rust backtrace on stderr from a destructor. Release
        // the handle instead — the thread is the parent's to reap.
        if crate::fork::forked_since(self.made_in) {
            let _ = self.shutdown_tx.take();
            let _ = self.thread_handle.take();
            return Err(SyncError::ForkedChild);
        }

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
                    // `JoinHandle` has no timed join, so a helper thread does the
                    // blocking join and reports through a channel. `recv_timeout`
                    // parks until the thread is actually down, so a shutdown that
                    // takes 1 ms costs 1 ms instead of being rounded up to the
                    // next tick of a sleep loop.
                    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
                    thread::spawn(move || {
                        // Fails only if the caller already timed out and dropped
                        // the receiver — that is how this thread learns nobody
                        // is listening, not an error worth reporting.
                        let _ = done_tx.send(thread_handle.join().is_ok());
                    });

                    match done_rx.recv_timeout(duration) {
                        Ok(true) => {}
                        Ok(false) => {
                            return Err(SyncError::DetachFailed {
                                message: "Runtime thread panicked".to_string(),
                            })
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            return Err(SyncError::DetachFailed {
                                message: format!(
                                    "Runtime thread did not shut down within {:?}",
                                    duration
                                ),
                            })
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            return Err(SyncError::DetachFailed {
                                message: "Failed to join helper thread".to_string(),
                            })
                        }
                    }
                }
                None => {
                    // Join without timeout
                    thread_handle.join().map_err(|_| SyncError::DetachFailed {
                        message: "Runtime thread panicked during shutdown".to_string(),
                    })?;
                }
            }
        }

        Ok(())
    }

    fn setup_background(
        builder: AimDbBuilder,
        mut shutdown_rx: mpsc::Receiver<ShutdownSignal>,
        db_tx: mpsc::Sender<Startup<Arc<AimDb>>>,
        handle_tx: mpsc::Sender<Startup<tokio::runtime::Handle>>,
    ) {
        // Create a new Tokio runtime for this thread
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let cause = format!("Failed to create Tokio runtime: {}", e);
                log_error!("{}", cause);
                let _ = handle_tx.blocking_send(Err(cause));
                return;
            }
        };
        // Get the runtime handle before moving into block_on
        let rt_handle = runtime.handle().clone();
        // Send the runtime handle to the main thread
        if handle_tx.blocking_send(Ok(rt_handle)).is_err() {
            log_error!("Failed to send runtime handle to main thread");
            return;
        }
        runtime.block_on(async move {
            // Build the database inside the async context
            let (db, runner) = match builder.build().await {
                Ok(d) => (Arc::new(d.0), d.1),
                Err(e) => {
                    let cause = format!("Failed to build database: {}", e);
                    log_error!("{}", cause);
                    let _ = db_tx.send(Err(cause)).await;
                    return;
                }
            };

            // Send the database to the main thread
            if db_tx.send(Ok(db.clone())).await.is_err() {
                log_error!("Failed to send database to main thread");
                return;
            }

            // Drive the runner until shutdown.
            // If runner.run() completes early (e.g. all tap futures finish),
            // we must NOT drop the runtime — tasks spawned via runtime_handle
            // would be aborted. Keep waiting for the explicit shutdown signal.
            tokio::select! {
                _ = runner.run() => { let _ = shutdown_rx.recv().await; }
                _ = shutdown_rx.recv() => {}
            }
        });
    }
}

impl Drop for AimDbHandle {
    /// Attempts graceful shutdown if `detach()` was not called.
    ///
    /// Logs a warning and attempts shutdown with a 5-second timeout.
    /// If shutdown fails, the runtime thread may be left running.
    fn drop(&mut self) {
        // A child's handle owns nothing that runs. Releasing it quietly is
        // correct; the warning below is for a *parent* that forgot to detach.
        if crate::fork::forked_since(self.made_in) {
            let _ = self.shutdown_tx.take();
            let _ = self.thread_handle.take();
            return;
        }

        if self.thread_handle.is_some() {
            log_warn!("Warning: AimDbHandle dropped without calling detach()");
            log_warn!("Attempting emergency shutdown with 5 second timeout");

            let timeout = Duration::from_secs(5);
            if let Err(e) = self.detach_internal(Some(timeout)) {
                log_error!("Error during emergency shutdown: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    #[allow(dead_code)]
    fn check() {
        assert_send::<crate::AimDbHandle>();
        assert_sync::<crate::AimDbHandle>();
    }
}
