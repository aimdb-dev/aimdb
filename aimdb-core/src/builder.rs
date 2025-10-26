//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, string::String, sync::Arc};

use crate::emitter::Emitter;
use crate::producer_consumer::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::{DbError, DbResult};

/// Internal database state
///
/// Holds the registry of typed records, indexed by `TypeId`.
pub struct AimDbInner {
    /// Map from TypeId to type-erased records (SPMC buffers for internal data flow)
    pub records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
}

/// Type alias for spawning closure
/// Captures the logic to spawn tap tasks for a specific record type
type SpawnFn = Box<
    dyn FnOnce(&Arc<dyn core::any::Any + Send + Sync>, &Arc<AimDb>) -> crate::DbResult<()>
        + Send
        + Sync,
>;

/// Type alias for the spawn callback stored in AimDb
/// This captures the concrete runtime type for spawning tasks
type SpawnCallback = Arc<
    dyn Fn(
            Box<dyn core::future::Future<Output = ()> + Send + 'static>,
        ) -> aimdb_executor::ExecutorResult<()>
        + Send
        + Sync,
>;

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
pub struct AimDbBuilder {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter (type-erased for storage)
    runtime: Option<Arc<dyn core::any::Any + Send + Sync>>,

    /// Spawn callback for automatic tap spawning
    spawn_callback: Option<SpawnCallback>,

    /// Connector pools indexed by scheme (mqtt, shmem, kafka, etc.)
    /// Used by .link() to route publishing requests to the correct protocol handler
    pub(crate) connector_pools: BTreeMap<String, Arc<dyn crate::pool::ConnectorPool>>,

    /// Spawning closures for automatic tap task spawning
    /// Each closure captures the concrete type information needed to spawn tasks
    spawn_fns: Vec<SpawnFn>,
}

impl AimDbBuilder {
    /// Creates a new database builder
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
            spawn_callback: None,
            connector_pools: BTreeMap::new(),
            spawn_fns: Vec::new(),
        }
    }

    /// Sets the runtime adapter
    pub fn with_runtime<R>(mut self, rt: Arc<R>) -> Self
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let rt_clone = rt.clone();

        // Store the spawn callback - this captures the concrete runtime type
        // and allows us to spawn tasks later without type erasure issues
        let spawn_callback = Arc::new(
            move |future: Box<dyn core::future::Future<Output = ()> + Send + 'static>| {
                use core::future::Future;
                use core::pin::Pin;
                use core::task::{Context, Poll};

                // Create a wrapper future that holds the Box<dyn Future>
                struct BoxedFuture(Box<dyn Future<Output = ()> + Send + 'static>);

                impl Future for BoxedFuture {
                    type Output = ();

                    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                        // SAFETY: We're just forwarding the poll to the inner future
                        // The Box provides a stable address, and we're careful not to move the inner future
                        let inner = unsafe { Pin::new_unchecked(&mut *self.0) };
                        inner.poll(cx)
                    }
                }

                rt_clone.spawn(BoxedFuture(future)).map(|_token| ())
            },
        );

        self.runtime = Some(rt as Arc<dyn core::any::Any + Send + Sync>);
        self.spawn_callback = Some(spawn_callback);
        self
    }

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
        f: impl for<'a> FnOnce(&'a mut RecordRegistrar<'a, T>),
    ) -> &mut Self
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        let entry = self
            .records
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(TypedRecord::<T>::new()));

        let rec = entry
            .as_typed_mut::<T>()
            .expect("type mismatch in record registry");

        let mut reg = RecordRegistrar {
            rec,
            connector_pools: &self.connector_pools,
        };
        f(&mut reg);

        // Capture a spawning closure for this record type
        // This closure will be called during build() to spawn tap tasks
        let type_id = TypeId::of::<T>();
        let spawn_fn: SpawnFn = Box::new(move |runtime, db| {
            // Get the record from the database
            let record = db.inner().records.get(&type_id).ok_or({
                #[cfg(feature = "std")]
                {
                    crate::DbError::RecordNotFound {
                        record_name: core::any::type_name::<T>().to_string(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    crate::DbError::RecordNotFound { _record_name: () }
                }
            })?;

            // Downcast to the concrete TypedRecord<T>
            let typed_record = record.as_typed::<T>().ok_or({
                #[cfg(feature = "std")]
                {
                    crate::DbError::RecordNotFound {
                        record_name: core::any::type_name::<T>().to_string(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    crate::DbError::RecordNotFound { _record_name: () }
                }
            })?;

            // Check if there are consumers to spawn
            if typed_record.consumer_count() > 0 {
                // We need to downcast the runtime to a Spawn trait object
                // The runtime is Arc<dyn Any + Send + Sync>, we need Arc<dyn Spawn>
                // This is the key insight: we can use Any::downcast_ref to get the concrete type

                // Try to extract a Spawn-capable runtime
                // Since we can't downcast trait objects, we need a different approach
                // Let's store the runtime with proper typing in Database::new()

                // Actually, the runtime in AimDb is already properly typed - we just
                // need to extract it. Let me use a helper method.
                typed_record.spawn_consumer_tasks_from_any(runtime, db)?;
            }

            // Check if there is a producer service to spawn
            if typed_record.has_producer_service() {
                typed_record.spawn_producer_service_from_any(runtime, db)?;
            }

            Ok(())
        });

        self.spawn_fns.push(spawn_fn);
        self
    }

    /// Registers a self-registering record type
    ///
    /// The record type must implement `RecordT`.
    pub fn register_record<R>(&mut self, cfg: &R::Config) -> &mut Self
    where
        R: RecordT,
    {
        self.configure::<R>(|reg| R::register(reg, cfg))
    }

    /// Builds the database
    ///
    /// Validates all records and constructs the final `AimDb` instance.
    ///
    /// **Automatic Tap Spawning:** This method automatically spawns all `.tap()` observer
    /// tasks that were registered during database configuration. No manual spawning needed!
    pub fn build(self) -> DbResult<AimDb> {
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
                    message: "runtime not set (use with_runtime or Database<A>::builder().build())"
                        .into(),
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
            inner,
            runtime: runtime.clone(),
            spawn_fn: self.spawn_callback.clone(),
        });

        #[cfg(feature = "tracing")]
        tracing::info!("Spawning {} tap observer task groups", self.spawn_fns.len());

        // Execute all spawning closures to automatically spawn tap tasks
        for spawn_fn in self.spawn_fns {
            spawn_fn(&runtime, &db)?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!("Automatic tap spawning complete");

        // Unwrap the Arc to return the owned AimDb
        // This is safe because we just created it and hold the only reference
        let db_owned = Arc::try_unwrap(db).unwrap_or_else(|arc| (*arc).clone());

        Ok(db_owned)
    }
}

impl Default for AimDbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer-consumer database
///
/// A database instance with type-safe record registration and cross-record
/// communication via the Emitter pattern. See `examples/` for usage.
pub struct AimDb {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter (type-erased for compatibility)
    runtime: Arc<dyn core::any::Any + Send + Sync>,

    /// Spawn callback for automatic tap spawning
    /// Takes a future and spawns it using the runtime
    spawn_fn: Option<SpawnCallback>,
}

impl Clone for AimDb {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
            spawn_fn: self.spawn_fn.clone(),
        }
    }
}

impl AimDb {
    /// Internal accessor for the inner state
    ///
    /// Used by adapter crates. Should not be used by application code.
    #[doc(hidden)]
    pub fn inner(&self) -> &Arc<AimDbInner> {
        &self.inner
    }

    /// Builds a database with a closure-based builder pattern
    pub fn build_with<R>(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder)) -> DbResult<Self>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let mut b = AimDbBuilder::new().with_runtime(rt);
        f(&mut b);
        b.build()
    }

    /// Returns an emitter for cross-record communication
    pub fn emitter(&self) -> Emitter {
        // Clone the Arc to get a new reference
        let runtime_clone = self.runtime.clone();
        // Create emitter with the cloned runtime (already type-erased)
        Emitter {
            runtime: runtime_clone,
            inner: self.inner.clone(),
        }
    }

    /// Spawns a task using the database's runtime adapter
    ///
    /// This is an internal method used by automatic tap spawning.
    /// It bridges the type-erased runtime to the Spawn trait.
    ///
    /// # Arguments
    /// * `future` - The future to spawn
    ///
    /// # Returns
    /// `DbResult<()>` - Ok if the task was spawned successfully
    #[doc(hidden)]
    pub fn spawn_task<F>(&self, future: F) -> DbResult<()>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        use crate::DbError;

        let spawn_fn = self.spawn_fn.as_ref().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: "No spawn function available - runtime may not support spawning"
                        .into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Box the future and call the spawn function
        spawn_fn(Box::new(future)).map_err(|e| {
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: format!("Failed to spawn task: {:?}", e),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                let _ = e;
                DbError::RuntimeError { _message: () }
            }
        })?;

        Ok(())
    }

    /// Produces a value for a record type
    ///
    /// Triggers the producer and all consumers for the given type.
    pub async fn produce<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        self.emitter().emit::<T>(value).await
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
        let typed_rec = rec.as_typed::<T>().ok_or({
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
    /// Returns a `Producer<T>` that can only produce values of type `T`.
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
    pub fn producer<T>(&self) -> crate::producer::Producer<T>
    where
        T: Send + 'static + Debug + Clone,
    {
        crate::producer::Producer::new(Arc::new(self.clone()))
    }

    /// Creates a type-safe consumer for a specific record type
    ///
    /// Returns a `Consumer<T>` that can only subscribe to values of type `T`.
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
    pub fn consumer<T>(&self) -> crate::consumer::Consumer<T>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        crate::consumer::Consumer::new(Arc::new(self.clone()))
    }

    // Note: producer_stats() was removed because producer is now an auto-spawned service
    // rather than a callback, so statistics are not tracked.
    // Note: consumer_stats() was removed because consumers are now FnOnce closures
    // that are consumed during spawning. Consumer statistics are no longer tracked
    // separately from producer statistics.

    /// Returns a reference to the runtime (downcasted)
    ///
    /// Returns `Some(&R)` if the runtime type matches, `None` otherwise.
    pub fn runtime<R: 'static>(&self) -> Option<&R> {
        self.runtime.downcast_ref::<R>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    struct TestConfig;

    impl RecordT for TestData {
        type Config = TestConfig;

        fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, _cfg: &Self::Config) {
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
}
