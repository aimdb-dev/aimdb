//! AimDB handle for managing the sync API runtime thread.

use crate::runtime::{Runtime, ShutdownSignal};
use crate::{SyncError, SyncResult};
use aimdb_core::{log_error, log_warn, AimDb, AimDbBuilder, DbError};
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
    /// The runtime thread and everything reached through it, shared with every
    /// producer and consumer made from this handle. See [`crate::runtime`].
    ///
    /// This is the only strong reference: producers and consumers hold a
    /// [`Weak`](alloc::sync::Weak), so the runtime dies with this handle and a
    /// producer that outlives it fails with
    /// [`SyncError::RuntimeShutdown`](crate::SyncError::RuntimeShutdown).
    /// [`detach`](Self::detach) is what stops the thread deliberately, without
    /// waiting for the survivors to finish.
    rt: Arc<Runtime>,

    /// What only the handle that started the thread may do: signal it, wait for
    /// it, join it.
    ///
    /// `None` once detached, or once a `fork` proved the thread is not this
    /// process's to reap. One field, so releasing it cannot be half-done — the
    /// previous shape was four loose fields and a release path that forgot one
    /// of them the moment a fifth was added.
    owned: Option<OwnedThread>,
}

/// The parts of a runtime thread that only its owner may touch.
struct OwnedThread {
    /// Joined by `detach`; released, never joined, by `Drop`.
    join: JoinHandle<()>,

    /// Commands the thread to stop. Distinct from dropping it: the thread also
    /// ends when every sender is gone, but *sending* ends it now, while
    /// producers and consumers still hold views of the runtime.
    shutdown: mpsc::Sender<ShutdownSignal>,

    /// Held open by the runtime thread for exactly as long as it runs.
    ///
    /// Nothing is ever sent on it. The thread moves the sender in and drops it
    /// on the way out — normal return, early return or panic alike — so a
    /// `Disconnected` here means "the thread is done" and a timeout means "it
    /// is still going". That is a timed join without a second thread to do the
    /// blocking, which is what `JoinHandle` does not provide.
    ///
    /// Behind a `Mutex` only to keep `AimDbHandle: Sync`, which `consumer()`
    /// relies on — a bare `Receiver` is `Send` but not `Sync`. It is never
    /// locked: every access takes it by value.
    alive: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

/// What the runtime thread reports back while starting up: the thing itself,
/// or why it could not be produced.
///
/// A `DbError` rather than a message, so the cause's classification survives
/// the trip into [`SyncError::AttachFailed`] instead of being flattened.
type Startup<T> = Result<T, DbError>;

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
        Some(Err(cause)) => Err(SyncError::AttachFailed { source: cause }),
        None => Err(SyncError::AttachFailed {
            source: DbError::runtime_error(format!(
                "runtime thread stopped before sending the {}",
                what
            )),
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

        // See `OwnedThread::alive`: never sent on, only dropped when the
        // thread ends. The flag beside it answers the same question without
        // blocking, which is what a producer on the publish path needs.
        let (alive_tx, thread_alive) = std::sync::mpsc::channel::<()>();

        // Spawn the runtime thread
        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(|| Self::setup_background(builder, shutdown_rx, db_tx, handle_tx, alive_tx))
            .map_err(|e| SyncError::AttachFailed {
                source: DbError::runtime_error(format!("Failed to spawn runtime thread: {}", e)),
            })?;

        // Both report the thread's own reason for failing rather than only the
        // fact that it failed.
        let runtime_handle = recv_startup(&mut handle_rx, "runtime handle")?;
        let db = recv_startup(&mut db_rx, "database")?;

        Ok(Self::assemble(
            runtime_handle,
            db,
            shutdown_tx,
            thread_handle,
            thread_alive,
        ))
    }

    pub(crate) fn new(db: AimDb) -> SyncResult<Self> {
        crate::fork::arm();

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownSignal>(1);

        // A channel rather than a mutex the caller polls — the same shape
        // `new_from_builder` has always used. See `recv_startup`.
        let (handle_tx, mut handle_rx) = mpsc::channel::<Startup<tokio::runtime::Handle>>(1);

        // See `OwnedThread::alive`.
        let (alive_tx, thread_alive) = std::sync::mpsc::channel::<()>();

        // Wrap database in Arc for sharing
        let db = Arc::new(db);

        // Spawn the runtime thread
        let thread_handle = thread::Builder::new()
            .name("aimdb-sync-runtime".to_string())
            .spawn(move || {
                // Moved in so it lives exactly as long as this thread does,
                // including if an early return below cuts things short.
                let _alive_tx = alive_tx;

                // Create a new Tokio runtime for this thread
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        // Report the reason before dying. Dropping the sender
                        // would already unblock the caller; this is what gives
                        // it something to print.
                        log_error!("Failed to create Tokio runtime: {}", e);
                        let _ = handle_tx.blocking_send(Err(DbError::runtime_error(format!(
                            "Failed to create Tokio runtime: {}",
                            e
                        ))));
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
                source: DbError::runtime_error(format!("Failed to spawn runtime thread: {}", e)),
            })?;

        let runtime_handle = recv_startup(&mut handle_rx, "runtime handle")?;

        Ok(Self::assemble(
            runtime_handle,
            db,
            shutdown_tx,
            thread_handle,
            thread_alive,
        ))
    }

    /// Both constructors end the same way: one shared [`Runtime`], one owned
    /// thread. Written once so they cannot drift.
    fn assemble(
        runtime_handle: tokio::runtime::Handle,
        db: Arc<AimDb>,
        shutdown: mpsc::Sender<ShutdownSignal>,
        join: JoinHandle<()>,
        alive: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self {
            rt: Arc::new(Runtime::new(runtime_handle, db)),
            owned: Some(OwnedThread {
                join,
                shutdown,
                alive: std::sync::Mutex::new(alive),
            }),
        }
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
        // No check here, and deliberately none. Creating a producer touches
        // nothing: not the database, not the runtime thread. `test_error_propagation`
        // pins that — an unregistered key yields a producer, and `set()` reports
        // the problem. A forked child is one more problem `set()` reports, via
        // the `db()` it must pass through; making `fork` the single exception to
        // this crate's own lazy-producer contract would be the odd thing.
        //
        // `consumer()` below does refuse in a child, because subscribing needs
        // the database and `db()` checks. That asymmetry is not new: an
        // unregistered key already fails there and not here.
        Ok(crate::SyncProducer::new(Arc::downgrade(&self.rt), key))
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
        let record_key = key.as_ref().to_string();
        // `db()` checks, so subscribing is gated the same way publishing is.
        let reader = self
            .rt
            .db()?
            .subscribe::<T>(&record_key)
            .map_err(SyncError::Db)?;
        Ok(crate::SyncConsumer::new(self.rt.view()?, reader))
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
    /// # What a timeout leaves behind
    ///
    /// `DetachFailed` from an expired timeout means the runtime thread had not
    /// finished in time — not that shutdown failed. Concretely:
    ///
    /// - The shutdown signal **was** delivered, so the thread stops on its own
    ///   and drops the database when it does. Nothing is stranded: no thread is
    ///   left parked waiting to reap it.
    /// - The handle is consumed either way, so there is nothing left to retry
    ///   with and no way to wait longer on this handle.
    /// - Any [`SyncProducer`](crate::SyncProducer) or
    ///   [`SyncConsumer`](crate::SyncConsumer) you still hold keeps working
    ///   until the thread stops, then fails with
    ///   [`SyncError::RuntimeShutdown`].
    /// - Resources are therefore released *eventually*, not by the time this
    ///   returns. Use [`detach`](Self::detach), which has no timeout, when you
    ///   need to know the thread is down.
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
    ///
    /// Deliberate shutdown: *sends* the signal rather than merely dropping a
    /// sender, so the thread stops at once rather than when the last sender
    /// goes. Producers and consumers hold only a
    /// [`Weak`](alloc::sync::Weak), so they cannot keep it alive; what they can
    /// do is still be mid-call when it stops, and they then fail with
    /// [`SyncError::RuntimeShutdown`]. That is the point — `detach` means "stop
    /// now", not "stop when everyone has finished".
    fn detach_internal(&mut self, timeout: Option<Duration>) -> SyncResult<()> {
        // A forked child holds a `JoinHandle` for a thread that does not exist
        // here, and joining it is not merely useless: it panics inside `std`
        // with "threads should not terminate unexpectedly", which for an FFI
        // caller means a Rust backtrace on stderr from a destructor. Release
        // it instead — the thread is the parent's to reap.
        if self.rt.check().is_err() {
            self.owned = None;
            return Err(SyncError::ForkedChild);
        }

        let Some(OwnedThread {
            join,
            shutdown,
            alive,
        }) = self.owned.take()
        else {
            return Ok(());
        };

        // Non-blocking. Failure means the thread has already stopped.
        let _ = shutdown.try_send(ShutdownSignal);

        if let Some(duration) = timeout {
            // `JoinHandle` has no timed join. Rather than park a helper thread
            // in `join()` — which could not be reclaimed when the wait expired,
            // stranding it for the life of the process — wait on the liveness
            // channel the runtime thread holds open. See `OwnedThread::alive`.
            //
            // Taken by value, so this cannot block and cannot fail; the
            // poisoned arm is unreachable because nothing ever locks it.
            let alive = alive.into_inner().unwrap_or_else(|e| e.into_inner());
            match alive.recv_timeout(duration) {
                // The thread dropped its sender, so it is on its way out and
                // the join below returns promptly.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}

                // Still running. Release the `JoinHandle` instead of blocking
                // on it: the shutdown signal was delivered, so the thread stops
                // on its own and drops the database with it.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    return Err(SyncError::DetachFailed {
                        message: format!("Runtime thread did not shut down within {:?}", duration),
                    });
                }

                // Nothing is ever sent on this channel.
                Ok(()) => {}
            }
        }

        join.join().map_err(|_| SyncError::DetachFailed {
            message: "Runtime thread panicked during shutdown".to_string(),
        })?;

        Ok(())
    }

    fn setup_background(
        builder: AimDbBuilder,
        mut shutdown_rx: mpsc::Receiver<ShutdownSignal>,
        db_tx: mpsc::Sender<Startup<Arc<AimDb>>>,
        handle_tx: mpsc::Sender<Startup<tokio::runtime::Handle>>,
        // Never sent on: dropped when this function returns, by any path.
        _alive_tx: std::sync::mpsc::Sender<()>,
    ) {
        // Create a new Tokio runtime for this thread
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                log_error!("Failed to create Tokio runtime: {}", e);
                let _ = handle_tx.blocking_send(Err(DbError::runtime_error(format!(
                    "Failed to create Tokio runtime: {}",
                    e
                ))));
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
                    // Sent as-is: this is the error whose kind is worth keeping.
                    log_error!("Failed to build database: {}", e);
                    let _ = db_tx.send(Err(e)).await;
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
    /// Signals shutdown and releases the thread. Never blocks.
    ///
    /// A destructor is the one place a failure cannot be reported — it can only
    /// log — so blocking here buys nothing the caller can act on. It also runs
    /// in places that must not stall: during unwinding, and inside a C++
    /// destructor when the handle is owned across an FFI boundary. The shutdown
    /// signal is what actually causes cleanup, and it is delivered either way;
    /// joining would only change when the caller learns it finished.
    ///
    /// Call [`detach`](Self::detach) if you need to know that it did.
    fn drop(&mut self) {
        // A forked child's handle owns nothing that runs here: signalling would
        // reach a thread this process never had. Release and say nothing.
        if self.rt.check().is_err() {
            self.owned = None;
            return;
        }

        if let Some(owned) = self.owned.take() {
            log_warn!("AimDbHandle dropped without calling detach()");
            log_warn!("Shutdown was signalled; the runtime thread stops on its own");
            // Non-blocking. Released rather than joined — see the note above.
            let _ = owned.shutdown.try_send(ShutdownSignal);
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
