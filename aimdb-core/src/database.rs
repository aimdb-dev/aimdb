//! AimDB Database Implementation
//!
//! This module provides the unified database implementation for AimDB, supporting async
//! in-memory storage with type-safe records and real-time synchronization across
//! MCU → edge → cloud environments.

use crate::{
    AimDb, AimDbBuilder, DbError, DbResult, Emitter, RecordT, RuntimeAdapter, RuntimeContext,
};
use aimdb_executor::Spawn;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

/// Database specification
///
/// Runtime-agnostic configuration for creating a database instance.
#[allow(dead_code)] // Used by adapter crates
pub struct DatabaseSpec<A: RuntimeAdapter> {
    pub(crate) aimdb_builder: AimDbBuilder,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter> DatabaseSpec<A> {
    /// Creates a new specification builder
    pub fn builder() -> DatabaseSpecBuilder<A> {
        DatabaseSpecBuilder::new()
    }
}

/// Builder for database specifications
///
/// Provides a fluent API for configuring databases with type-safe record registration.
pub struct DatabaseSpecBuilder<A: RuntimeAdapter> {
    aimdb_builder: AimDbBuilder,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter> DatabaseSpecBuilder<A> {
    /// Creates a new database specification builder
    fn new() -> Self {
        Self {
            aimdb_builder: AimDbBuilder::new(),
            _phantom: core::marker::PhantomData,
        }
    }

    /// Registers a type-safe record with configuration
    ///
    /// # Example
    /// ```rust,ignore
    /// let builder = Database::<TokioAdapter>::builder()
    ///     .record::<SensorData>(&sensor_cfg)
    ///     .record::<Alerts>(&alert_cfg);
    /// ```
    pub fn record<T: RecordT>(mut self, cfg: &T::Config) -> Self {
        #[cfg(feature = "tracing")]
        tracing::debug!("Registering typed record: {}", core::any::type_name::<T>());

        self.aimdb_builder.register_record::<T>(cfg);
        self
    }

    /// Internal method to convert builder to spec
    ///
    /// Used by adapter-specific build methods.
    #[allow(dead_code)] // Used by adapter crates
    pub fn into_spec(self) -> DatabaseSpec<A> {
        #[cfg(feature = "tracing")]
        tracing::info!("Building database spec with typed records");

        DatabaseSpec {
            aimdb_builder: self.aimdb_builder,
            _phantom: core::marker::PhantomData,
        }
    }
}

/// AimDB Database implementation
///
/// Unified database combining runtime adapter management with type-safe record
/// registration and producer-consumer patterns. See `examples/` for usage patterns.
pub struct Database<A: RuntimeAdapter> {
    adapter: A,
    aimdb: AimDb,
}

impl<A: RuntimeAdapter> Database<A> {
    /// Internal accessor for the AimDb instance
    ///
    /// Used by adapter crates. Should not be used by application code.
    #[doc(hidden)]
    pub fn inner_aimdb(&self) -> &AimDb {
        &self.aimdb
    }

    /// Creates a new database builder
    ///
    /// Use this to configure a database with type-safe records. See `examples/` for usage.
    pub fn builder() -> DatabaseSpecBuilder<A> {
        DatabaseSpecBuilder::new()
    }

    /// Internal constructor for creating a database from a spec
    ///
    /// Called by adapter-specific build methods. Should not be used directly.
    #[allow(dead_code)] // Used by adapter crates
    pub fn new(adapter: A, spec: DatabaseSpec<A>) -> DbResult<Self>
    where
        A: Clone + aimdb_executor::Spawn + 'static,
    {
        #[cfg(feature = "tracing")]
        tracing::info!("Initializing unified database with typed records");

        // Build the AimDb with the runtime
        let runtime = Arc::new(adapter.clone());
        let aimdb = spec.aimdb_builder.with_runtime(runtime).build()?;

        Ok(Self { adapter, aimdb })
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

    /// Gets the emitter for cross-record communication
    ///
    /// # Example
    /// ```rust,ignore
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) {
    /// let emitter = db.emitter();
    /// emitter.emit(SensorData { temp: 23.5 }).await;
    /// # }
    /// ```
    pub fn emitter(&self) -> Emitter {
        self.aimdb.emitter()
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
        let typed_record = record.as_typed::<T>().ok_or(DbError::RecordNotFound {
            record_name: core::any::type_name::<T>().to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let typed_record = record
            .as_typed::<T>()
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
    /// // Pass ctx to services - they can use ctx.emitter()
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
                .with_emitter(self.emitter())
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
