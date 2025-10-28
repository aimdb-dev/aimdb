//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;

use alloc::collections::BTreeMap;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, string::String, sync::Arc};

use crate::typed_api::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::{DbError, DbResult};

/// Marker type for untyped builder (before runtime is set)
pub struct NoRuntime;

/// Internal database state
///
/// Holds the registry of typed records, indexed by `TypeId`.
pub struct AimDbInner {
    /// Map from TypeId to type-erased records (SPMC buffers for internal data flow)
    pub records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
}

impl AimDbInner {
    /// Helper to get a typed record from the registry
    ///
    /// This encapsulates the common pattern of:
    /// 1. Getting TypeId for type T
    /// 2. Looking up the record in the map
    /// 3. Downcasting to the typed record
    pub fn get_typed_record<T, R>(&self) -> DbResult<&TypedRecord<T, R>>
    where
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::Spawn + 'static,
    {
        use crate::typed_record::AnyRecordExt;

        let type_id = TypeId::of::<T>();

        #[cfg(feature = "std")]
        let record = self.records.get(&type_id).ok_or(DbError::RecordNotFound {
            record_name: core::any::type_name::<T>().to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let record = self
            .records
            .get(&type_id)
            .ok_or(DbError::RecordNotFound { _record_name: () })?;

        #[cfg(feature = "std")]
        let typed_record = record.as_typed::<T, R>().ok_or(DbError::InvalidOperation {
            operation: "get_typed_record".to_string(),
            reason: "type mismatch during downcast".to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let typed_record = record.as_typed::<T, R>().ok_or(DbError::InvalidOperation {
            _operation: (),
            _reason: (),
        })?;

        Ok(typed_record)
    }
}

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
/// Use `.runtime()` to set the runtime and transition to a typed builder.
pub struct AimDbBuilder<R = NoRuntime> {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter
    runtime: Option<Arc<R>>,

    /// Connectors indexed by scheme (mqtt, shmem, kafka, etc.)
    pub(crate) connectors: BTreeMap<String, Arc<dyn crate::transport::Connector>>,

    /// Spawn functions indexed by TypeId
    spawn_fns: BTreeMap<TypeId, Box<dyn core::any::Any + Send>>,

    /// PhantomData to track the runtime type parameter
    _phantom: PhantomData<R>,
}

impl AimDbBuilder<NoRuntime> {
    /// Creates a new database builder without a runtime
    ///
    /// Call `.runtime()` to set the runtime adapter.
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
            connectors: BTreeMap::new(),
            spawn_fns: BTreeMap::new(),
            _phantom: PhantomData,
        }
    }

    /// Sets the runtime adapter
    ///
    /// This transitions the builder from untyped to typed with concrete runtime `R`.
    pub fn runtime<R>(self, rt: Arc<R>) -> AimDbBuilder<R>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        AimDbBuilder {
            records: self.records,
            runtime: Some(rt),
            connectors: self.connectors,
            spawn_fns: BTreeMap::new(),
            _phantom: PhantomData,
        }
    }
}

impl<R> AimDbBuilder<R>
where
    R: aimdb_executor::Spawn + 'static,
{
    /// Registers a connector for a specific URL scheme
    ///
    /// The scheme (e.g., "mqtt", "shmem", "kafka") determines how `.link()` URLs
    /// are routed. Each scheme can have ONE connector, which manages connections to
    /// a specific endpoint (broker, segment, cluster, etc.).
    ///
    /// # Arguments
    /// * `scheme` - URL scheme without "://" (e.g., "mqtt", "shmem", "kafka")
    /// * `connector` - Connector implementing the Connector trait
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_mqtt_connector::MqttConnector;
    /// use std::sync::Arc;
    ///
    /// let mqtt_connector = MqttConnector::new("mqtt://broker.local:1883").await?;
    /// let shmem_connector = ShmemConnector::new("/dev/shm/aimdb");
    ///
    /// let builder = AimDbBuilder::new()
    ///     .runtime(runtime)
    ///     .with_connector("mqtt", Arc::new(mqtt_connector))
    ///     .with_connector("shmem", Arc::new(shmem_connector));
    ///
    /// // Now .link() can route to either:
    /// //   .link("mqtt://sensors/temp")  → mqtt_connector
    /// //   .link("shmem://temp_data")    → shmem_connector
    /// ```
    pub fn with_connector(
        mut self,
        scheme: impl Into<String>,
        connector: Arc<dyn crate::transport::Connector>,
    ) -> Self {
        self.connectors.insert(scheme.into(), connector);
        self
    }

    /// Configures a record type manually
    ///
    /// Low-level method for advanced use cases. Most users should use `register_record` instead.
    pub fn configure<T>(
        &mut self,
        f: impl for<'a> FnOnce(&'a mut RecordRegistrar<'a, T, R>),
    ) -> &mut Self
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        let entry = self
            .records
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(TypedRecord::<T, R>::new()));

        let rec = entry
            .as_typed_mut::<T, R>()
            .expect("type mismatch in record registry");

        let mut reg = RecordRegistrar {
            rec,
            connectors: &self.connectors,
        };
        f(&mut reg);

        // Store a spawn function that captures the concrete type T
        let type_id = TypeId::of::<T>();
        #[allow(clippy::type_complexity)]
        let spawn_fn: Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>) -> DbResult<()> + Send> =
            Box::new(move |runtime: &Arc<R>, db: &Arc<AimDb<R>>| {
                // Use RecordSpawner to spawn tasks for this record type
                use crate::typed_record::RecordSpawner;

                let typed_record = db.inner().get_typed_record::<T, R>()?;
                RecordSpawner::<T>::spawn_all_tasks(typed_record, runtime, db)
            });

        // Store the spawn function (type-erased in Box<dyn Any>)
        self.spawn_fns.insert(type_id, Box::new(spawn_fn));

        self
    }

    /// Registers a self-registering record type
    ///
    /// The record type must implement `RecordT<R>`.
    pub fn register_record<T>(&mut self, cfg: &T::Config) -> &mut Self
    where
        T: RecordT<R>,
    {
        self.configure::<T>(|reg| T::register(reg, cfg))
    }

    /// Runs the database indefinitely (never returns)
    ///
    /// This method builds the database, spawns all producer and consumer tasks, and then
    /// parks the current task indefinitely. This is the primary way to run AimDB services.
    ///
    /// All logic runs in background tasks via producers, consumers, and connectors. The
    /// application continues until interrupted (e.g., Ctrl+C).
    ///
    /// # Returns
    /// `DbResult<()>` - Ok when database starts successfully, then parks forever
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[tokio::main]
    /// async fn main() -> DbResult<()> {
    ///     AimDbBuilder::new()
    ///         .runtime(Arc::new(TokioAdapter::new()?))
    ///         .configure::<MyData>(|reg| {
    ///             reg.with_buffer(BufferCfg::SpmcRing { capacity: 100 })
    ///                .with_source(my_producer)
    ///                .with_tap(my_consumer);
    ///         })
    ///         .run().await  // Runs forever
    /// }
    /// ```
    pub async fn run(self) -> DbResult<()> {
        // Build the database and spawn all tasks
        let _db = self.build()?;

        #[cfg(feature = "tracing")]
        tracing::info!("Database running, background tasks active. Press Ctrl+C to stop.");

        // Park indefinitely - the background tasks will continue running
        // The database handle is kept alive here to prevent dropping it
        core::future::pending::<()>().await;

        Ok(())
    }

    /// Builds the database and returns the handle (advanced use)
    ///
    /// Use this when you need programmatic access to the database handle for
    /// manual subscriptions or production. For typical services, use `.run().await` instead.
    ///
    /// **Automatic Task Spawning:** This method spawns all producer services and
    /// `.tap()` observer tasks that were registered during configuration.
    ///
    /// # Returns
    /// `DbResult<AimDb<R>>` - The database instance
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = AimDbBuilder::new()
    ///     .runtime(Arc::new(TokioAdapter::new()?))
    ///     .configure::<MyData>(|reg| { /* ... */ })
    ///     .build()?;
    ///
    /// // Manually subscribe or produce
    /// let mut reader = db.subscribe::<MyData>()?;
    /// ```
    pub fn build(self) -> DbResult<AimDb<R>> {
        use crate::DbError;

        // Validate all records
        for record in self.records.values() {
            record.validate().map_err(|_msg| {
                #[cfg(feature = "std")]
                {
                    DbError::RuntimeError {
                        message: format!("Record validation failed: {}", _msg),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    DbError::RuntimeError { _message: () }
                }
            })?;
        }

        // Ensure runtime is set
        let runtime = self.runtime.ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: "runtime not set (use .runtime())".into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        let inner = Arc::new(AimDbInner {
            records: self.records,
        });

        let db = Arc::new(AimDb {
            inner: inner.clone(),
            runtime: runtime.clone(),
            _phantom: PhantomData,
        });

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Spawning producer services and tap observers for {} record types",
            self.spawn_fns.len()
        );

        // Execute spawn functions for each record type
        for (_type_id, spawn_fn_any) in self.spawn_fns {
            // Downcast from Box<dyn Any> back to the concrete spawn function type
            type SpawnFnType<R> = Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>) -> DbResult<()> + Send>;

            let spawn_fn = spawn_fn_any
                .downcast::<SpawnFnType<R>>()
                .expect("spawn function type mismatch");

            // Execute the spawn function
            (*spawn_fn)(&runtime, &db)?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!("Automatic spawning complete");

        // Unwrap the Arc to return the owned AimDb
        // This is safe because we just created it and hold the only reference
        let db_owned = Arc::try_unwrap(db).unwrap_or_else(|arc| (*arc).clone());

        Ok(db_owned)
    }
}

impl Default for AimDbBuilder<NoRuntime> {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer-consumer database
///
/// A database instance with type-safe record registration and cross-record
/// communication via the Emitter pattern. The type parameter `R` represents
/// the runtime adapter (e.g., TokioAdapter, EmbassyAdapter).
///
/// See `examples/` for usage.
///
/// # Examples
///
/// ```rust,ignore
/// use aimdb_tokio_adapter::TokioAdapter;
///
/// let runtime = Arc::new(TokioAdapter);
/// let db: AimDb<TokioAdapter> = AimDbBuilder::new()
///     .runtime(runtime)
///     .register_record::<Temperature>(&TemperatureConfig)
///     .build()?;
/// ```
pub struct AimDb<R: aimdb_executor::Spawn + 'static> {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter with concrete type
    runtime: Arc<R>,

    /// PhantomData to track the runtime type parameter
    _phantom: PhantomData<R>,
}

impl<R: aimdb_executor::Spawn + 'static> Clone for AimDb<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<R: aimdb_executor::Spawn + 'static> AimDb<R> {
    /// Internal accessor for the inner state
    ///
    /// Used by adapter crates and internal spawning logic.
    #[doc(hidden)]
    pub fn inner(&self) -> &Arc<AimDbInner> {
        &self.inner
    }

    /// Builds a database with a closure-based builder pattern
    pub async fn build_with(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder<R>)) -> DbResult<()> {
        let mut b = AimDbBuilder::new().runtime(rt);
        f(&mut b);
        b.run().await
    }

    /// Spawns a task using the database's runtime adapter
    ///
    /// This method provides direct access to the runtime's spawn capability.
    ///
    /// # Arguments
    /// * `future` - The future to spawn
    ///
    /// # Returns
    /// `DbResult<()>` - Ok if the task was spawned successfully
    pub fn spawn_task<F>(&self, future: F) -> DbResult<()>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future).map_err(DbError::from)?;
        Ok(())
    }

    /// Produces a value for a record type
    ///
    /// Writes the value to the record's buffer and triggers all consumers.
    pub async fn produce<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Get the typed record using the helper
        let typed_rec = self.inner.get_typed_record::<T, R>()?;

        // Produce the value directly to the buffer
        typed_rec.produce(value).await;
        Ok(())
    }

    /// Subscribes to a record type's buffer
    ///
    /// Creates a subscription to the configured buffer for the given record type.
    /// Returns a boxed reader for receiving values asynchronously.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut reader = db.subscribe::<Temperature>()?;
    ///
    /// loop {
    ///     match reader.recv().await {
    ///         Ok(temp) => println!("Temperature: {:.1}°C", temp.celsius),
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub fn subscribe<T>(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Get the typed record using the helper
        let typed_rec = self.inner.get_typed_record::<T, R>()?;

        // Subscribe to the buffer
        typed_rec.subscribe()
    }

    /// Creates a type-safe producer for a specific record type
    ///
    /// Returns a `Producer<T, R>` that can only produce values of type `T`.
    /// This is the recommended way to pass database access to producer services,
    /// following the principle of least privilege.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = builder.build()?;
    /// let temp_producer = db.producer::<Temperature>();
    ///
    /// // Pass to service - it can only produce Temperature values
    /// runtime.spawn(temperature_service(ctx, temp_producer)).unwrap();
    /// ```
    pub fn producer<T>(&self) -> crate::typed_api::Producer<T, R>
    where
        T: Send + 'static + Debug + Clone,
    {
        crate::typed_api::Producer::new(Arc::new(self.clone()))
    }

    /// Creates a type-safe consumer for a specific record type
    ///
    /// Returns a `Consumer<T, R>` that can only subscribe to values of type `T`.
    /// This is the recommended way to pass database access to consumer services,
    /// following the principle of least privilege.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = builder.build()?;
    /// let temp_consumer = db.consumer::<Temperature>();
    ///
    /// // Pass to service - it can only consume Temperature values
    /// runtime.spawn(temperature_monitor(ctx, temp_consumer)).unwrap();
    /// ```
    pub fn consumer<T>(&self) -> crate::typed_api::Consumer<T, R>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        crate::typed_api::Consumer::new(Arc::new(self.clone()))
    }

    /// Returns a reference to the runtime adapter
    ///
    /// Provides direct access to the concrete runtime type.
    pub fn runtime(&self) -> &R {
        &self.runtime
    }
}

#[cfg(test)]
mod tests {
    // NOTE: Tests commented out because RecordT<R> now requires R: aimdb_executor::Spawn,
    // and we can't use () as a dummy runtime. See examples/ for working tests with real runtimes.

    /*
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    struct TestConfig;

    impl RecordT<R> for TestData {
        type Config = TestConfig;

        fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self, R>, _cfg: &Self::Config) {
            reg.source(|_prod, _ctx| async {}).tap(|_consumer| async {});
        }
    }

    #[test]
    fn test_builder_basic() {
        let mut builder = AimDbBuilder::new();
        builder.register_record::<TestData>(&TestConfig);

        // Should have one record registered
        assert_eq!(builder.records.len(), 1);
    }

    #[test]
    fn test_builder_configure() {
        let mut builder = AimDbBuilder::new();
        builder.configure::<TestData>(|reg| {
            reg.source(|_prod, _ctx| async {}).tap(|_consumer| async {});
        });

        assert_eq!(builder.records.len(), 1);
    }
    */
}
