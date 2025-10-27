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

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
/// Use `.with_runtime()` to set the runtime and transition to a typed builder.
pub struct AimDbBuilder<R = NoRuntime> {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter
    runtime: Option<Arc<R>>,

    /// Connector pools indexed by scheme (mqtt, shmem, kafka, etc.)
    pub(crate) connector_pools: BTreeMap<String, Arc<dyn crate::pool::ConnectorPool>>,

    /// Spawn functions indexed by TypeId
    spawn_fns: BTreeMap<TypeId, Box<dyn core::any::Any + Send>>,

    /// PhantomData to track the runtime type parameter
    _phantom: PhantomData<R>,
}

impl AimDbBuilder<NoRuntime> {
    /// Creates a new database builder without a runtime
    ///
    /// Call `.with_runtime()` to set the runtime adapter.
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
            connector_pools: BTreeMap::new(),
            spawn_fns: BTreeMap::new(),
            _phantom: PhantomData,
        }
    }

    /// Sets the runtime adapter
    ///
    /// This transitions the builder from untyped to typed with concrete runtime `R`.
    pub fn with_runtime<R>(self, rt: Arc<R>) -> AimDbBuilder<R>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        AimDbBuilder {
            records: self.records,
            runtime: Some(rt),
            connector_pools: self.connector_pools,
            spawn_fns: BTreeMap::new(),
            _phantom: PhantomData,
        }
    }
}

impl<R> AimDbBuilder<R>
where
    R: aimdb_executor::Spawn + 'static,
{
    /// Registers a connector pool for a specific URL scheme
    ///
    /// The scheme (e.g., "mqtt", "shmem", "kafka") determines how `.link()` URLs
    /// are routed. Each scheme can have ONE pool, which manages connections to
    /// a specific endpoint (broker, segment, cluster, etc.).
    ///
    /// # Arguments
    /// * `scheme` - URL scheme without "://" (e.g., "mqtt", "shmem", "kafka")
    /// * `pool` - Connector pool implementing the ConnectorPool trait
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_mqtt_connector::MqttClientPool;
    /// use std::sync::Arc;
    ///
    /// let mqtt_pool = MqttClientPool::new("broker.local:1883");
    /// let shmem_pool = ShmemConnectorPool::new("/dev/shm/aimdb");
    ///
    /// let builder = AimDbBuilder::new()
    ///     .with_runtime(runtime)
    ///     .with_connector_pool("mqtt", Arc::new(mqtt_pool))
    ///     .with_connector_pool("shmem", Arc::new(shmem_pool));
    ///
    /// // Now .link() can route to either:
    /// //   .link("mqtt://sensors/temp")  → mqtt_pool
    /// //   .link("shmem://temp_data")    → shmem_pool
    /// ```
    pub fn with_connector_pool(
        mut self,
        scheme: impl Into<String>,
        pool: Arc<dyn crate::pool::ConnectorPool>,
    ) -> Self {
        self.connector_pools.insert(scheme.into(), pool);
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
            connector_pools: &self.connector_pools,
        };
        f(&mut reg);

        // Store a spawn function that captures the concrete type T
        let type_id = TypeId::of::<T>();
        #[allow(clippy::type_complexity)]
        let spawn_fn: Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>) -> DbResult<()> + Send> =
            Box::new(move |runtime: &Arc<R>, db: &Arc<AimDb<R>>| {
                // Use RecordSpawner to spawn tasks for this record type
                use crate::typed_record::RecordSpawner;

                let record = db.inner().records.get(&type_id).ok_or({
                    #[cfg(feature = "std")]
                    {
                        DbError::RecordNotFound {
                            record_name: core::any::type_name::<T>().to_string(),
                        }
                    }
                    #[cfg(not(feature = "std"))]
                    {
                        DbError::RecordNotFound { _record_name: () }
                    }
                })?;

                RecordSpawner::<T>::spawn_all_tasks(record.as_ref(), runtime, db)
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

    /// Builds the database
    ///
    /// Validates all records and constructs the final `AimDb<R>` instance.
    ///
    /// **Automatic Tap Spawning:** This method automatically spawns all `.tap()` observer
    /// tasks that were registered during database configuration. No manual spawning needed!
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
                    message: "runtime not set (use with_runtime)".into(),
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
///     .with_runtime(runtime)
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
    pub fn build_with(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder<R>)) -> DbResult<Self> {
        let mut b = AimDbBuilder::new().with_runtime(rt);
        f(&mut b);
        b.build()
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
        use crate::typed_record::AnyRecordExt;
        use core::any::TypeId;

        // Get the record for type T
        let type_id = TypeId::of::<T>();
        let rec = self.inner.records.get(&type_id).ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RecordNotFound {
                    record_name: core::any::type_name::<T>().to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RecordNotFound { _record_name: () }
            }
        })?;

        // Downcast to typed record
        let typed_rec = rec.as_typed::<T, R>().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::InvalidOperation {
                    operation: "produce".to_string(),
                    reason: "type mismatch".to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::InvalidOperation {
                    _operation: (),
                    _reason: (),
                }
            }
        })?;

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
        use crate::typed_record::AnyRecordExt;
        use core::any::TypeId;

        // Get the record for type T
        let type_id = TypeId::of::<T>();
        let rec = self.inner.records.get(&type_id).ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RecordNotFound {
                    record_name: core::any::type_name::<T>().to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RecordNotFound { _record_name: () }
            }
        })?;

        // Downcast to typed record
        let typed_rec = rec.as_typed::<T, R>().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::InvalidOperation {
                    operation: "subscribe".to_string(),
                    reason: "type mismatch".to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::InvalidOperation {
                    _operation: (),
                    _reason: (),
                }
            }
        })?;

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

    // Note: producer_stats() was removed because producer is now an auto-spawned service
    // rather than a callback, so statistics are not tracked.
    // Note: consumer_stats() was removed because consumers are now FnOnce closures
    // that are consumed during spawning. Consumer statistics are no longer tracked
    // separately from producer statistics.

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
