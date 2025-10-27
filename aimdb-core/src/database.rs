//! AimDB Database Implementation
//!
//! This module provides the unified database implementation for AimDB, supporting async
//! in-memory storage with type-safe records and real-time synchronization across
//! MCU → edge → cloud environments.

use crate::{AimDb, DbError, DbResult, RuntimeAdapter, RuntimeContext};
use aimdb_executor::Spawn;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

#[cfg(feature = "std")]
use std::boxed::Box;

/// AimDB Database implementation
///
/// Unified database combining runtime adapter management with type-safe record
/// registration and producer-consumer patterns. See `examples/` for usage patterns.
///
/// This is a thin wrapper around `AimDb<R>` that adds adapter-specific functionality.
/// Most users should use `AimDbBuilder` directly to create databases.
pub struct Database<A: RuntimeAdapter + aimdb_executor::Spawn + 'static> {
    adapter: A,
    aimdb: AimDb<A>,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter + aimdb_executor::Spawn + 'static> Database<A> {
    /// Internal accessor for the AimDb instance
    ///
    /// Used by adapter crates. Should not be used by application code.
    #[doc(hidden)]
    pub fn inner_aimdb(&self) -> &AimDb<A> {
        &self.aimdb
    }

    /// Creates a new database from adapter and AimDb
    ///
    /// # Arguments
    /// * `adapter` - The runtime adapter
    /// * `aimdb` - The configured AimDb instance
    ///
    /// Most users should use `AimDbBuilder` directly instead of this constructor.
    pub fn new(adapter: A, aimdb: AimDb<A>) -> DbResult<Self> {
        #[cfg(feature = "tracing")]
        tracing::info!("Initializing unified database with typed records");

        Ok(Self {
            adapter,
            aimdb,
            _phantom: core::marker::PhantomData,
        })
    }

    /// Gets a reference to the runtime adapter
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: Database<A>) {
    /// let adapter = db.adapter();
    /// // Use adapter directly
    /// # }
    /// # }
    /// ```
    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    /// Produces typed data to the record's producer pipeline
    ///
    /// # Example
    /// ```rust,ignore
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) -> aimdb_core::DbResult<()> {
    /// db.produce(SensorData { temp: 23.5 }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn produce<T>(&self, data: T) -> DbResult<()>
    where
        T: Send + 'static + Clone + core::fmt::Debug,
    {
        self.aimdb.produce(data).await
    }

    /// Subscribes to a record type's buffer
    ///
    /// Creates a subscription to the configured buffer for the given record type.
    /// Returns a boxed reader for receiving values asynchronously.
    ///
    /// # Example
    /// ```rust,ignore
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) -> aimdb_core::DbResult<()> {
    /// let mut reader = db.subscribe::<SensorData>()?;
    ///
    /// loop {
    ///     match reader.recv().await {
    ///         Ok(data) => println!("Received: {:?}", data),
    ///         Err(e) => {
    ///             eprintln!("Error: {:?}", e);
    ///             break;
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe<T>(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        use crate::typed_record::AnyRecordExt;
        use core::any::TypeId;

        // Get the record for type T
        let type_id = TypeId::of::<T>();

        #[cfg(feature = "std")]
        let record = self
            .aimdb
            .inner()
            .records
            .get(&type_id)
            .ok_or(DbError::RecordNotFound {
                record_name: core::any::type_name::<T>().to_string(),
            })?;

        #[cfg(not(feature = "std"))]
        let record = self
            .aimdb
            .inner()
            .records
            .get(&type_id)
            .ok_or(DbError::RecordNotFound { _record_name: () })?;

        // Downcast to typed record
        #[cfg(feature = "std")]
        let typed_record = record.as_typed::<T, A>().ok_or(DbError::RecordNotFound {
            record_name: core::any::type_name::<T>().to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let typed_record = record
            .as_typed::<T, A>()
            .ok_or(DbError::RecordNotFound { _record_name: () })?;

        // Get the buffer
        #[cfg(feature = "std")]
        let buffer = typed_record.buffer().ok_or(DbError::InvalidOperation {
            operation: "subscribe".to_string(),
            reason: format!(
                "No buffer configured for record type {}. Use RecordRegistrar::buffer() to configure a buffer.",
                core::any::type_name::<T>()
            ),
        })?;

        #[cfg(not(feature = "std"))]
        let buffer = typed_record.buffer().ok_or(DbError::InvalidOperation {
            _operation: (),
            _reason: (),
        })?;

        // DynBuffer provides subscribe_boxed() - universal across all runtimes!
        Ok(buffer.subscribe_boxed())
    }

    // Note: producer_stats() and consumer_stats() were removed because:
    // - Producers are now auto-spawned services rather than callbacks
    // - Consumers are FnOnce closures consumed during spawning
    // Statistics are no longer tracked for these components.

    /// Creates a RuntimeContext for this database
    ///
    /// Provides services with access to runtime capabilities (timing, logging) plus the emitter.
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # async fn example<A: aimdb_executor::Runtime + Clone>(db: Database<A>) {
    /// let ctx = db.context();
    /// // Pass ctx to services
    /// # }
    /// # }
    /// ```
    pub fn context(&self) -> RuntimeContext<A>
    where
        A: aimdb_executor::Runtime + Clone,
    {
        #[cfg(feature = "std")]
        {
            RuntimeContext::from_arc(std::sync::Arc::new(self.adapter.clone()))
        }
        #[cfg(not(feature = "std"))]
        {
            // For no_std, we need a static reference - this would typically be handled
            // by the caller storing the adapter in a static cell first
            // For now, we'll document this limitation
            panic!("context() not supported in no_std without a static reference. To use context(), store your adapter in a static cell (e.g., StaticCell from portable-atomic or embassy-sync), or use adapter() directly.")
        }
    }
}

// Spawn implementation for databases with spawn-capable adapters
impl<A> Database<A>
where
    A: RuntimeAdapter + Spawn,
{
    /// Spawns a service on the database's runtime
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # use aimdb_executor::{Runtime, Spawn};
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// async fn my_service<R: Runtime>(ctx: aimdb_core::RuntimeContext<R>) -> aimdb_core::DbResult<()> {
    ///     // Service implementation
    ///     Ok(())
    /// }
    ///
    /// # async fn example<A: Runtime + Spawn>(db: Database<A>) -> aimdb_core::DbResult<()> {
    /// let ctx = db.context();
    /// db.spawn(async move {
    ///     if let Err(e) = my_service(ctx).await {
    ///         eprintln!("Service error: {:?}", e);
    ///     }
    /// })?;
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn spawn<F>(&self, future: F) -> DbResult<()>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        #[cfg(feature = "tracing")]
        tracing::debug!("Spawning service on database runtime");

        self.adapter.spawn(future).map_err(DbError::from)?;
        Ok(())
    }
}
