//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;

extern crate alloc;

use alloc::collections::BTreeMap;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(not(feature = "std"))]
use spin::Mutex;

#[cfg(feature = "std")]
use std::sync::Mutex;

use crate::emitter::Emitter;
use crate::outbox::AnySender;
use crate::producer_consumer::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::DbResult;
use crate::RuntimeAdapter;

/// Internal database state
///
/// Holds the registry of typed records and outboxes, indexed by `TypeId`.
pub struct AimDbInner {
    /// Map from TypeId to type-erased records (SPMC buffers for internal data flow)
    pub records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Map from payload TypeId to MPSC sender (for outboxes to external systems)
    ///
    /// This registry stores type-erased senders for outbound message queues.
    /// Unlike records (which use SPMC for internal consumers), outboxes use
    /// MPSC for multi-producer access to single external system workers.
    ///
    /// # Design Rationale
    ///
    /// - Separate from `records` - outboxes are not records (different pattern)
    /// - Keyed by payload `TypeId` - ensures type safety at runtime  
    /// - `Arc<Mutex<_>>` - allows initialization after `AimDb` creation
    /// - `Box<dyn AnySender>` - type erasure for heterogeneous channel storage
    pub(crate) outboxes: Arc<Mutex<BTreeMap<TypeId, Box<dyn AnySender>>>>,
}

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe
/// record registration.
///
/// # Design Philosophy
///
/// - **Type Safety**: Records are identified by TypeId, not strings
/// - **Validation**: Ensures all records have valid producer/consumer setup
/// - **Runtime Agnostic**: Works with any Runtime implementation
/// - **Builder Pattern**: Fluent API for construction
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::experimental::AimDb;
/// use aimdb_tokio_adapter::TokioAdapter;
///
/// let runtime = Arc::new(TokioAdapter::new()?);
///
/// let db = AimDb::build_with(runtime, |b| {
///     b.register_record::<SensorData>(&sensor_cfg);
///     b.register_record::<Alert>(&alert_cfg);
/// })?;
/// ```
pub struct AimDbBuilder {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter (type-erased for storage)
    runtime: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

impl AimDbBuilder {
    /// Creates a new database builder
    ///
    /// # Returns
    /// An empty `AimDbBuilder`
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
        }
    }

    /// Sets the runtime adapter
    ///
    /// # Arguments
    /// * `rt` - The runtime adapter to use
    ///
    /// # Returns
    /// `Self` for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = AimDbBuilder::new()
    ///     .with_runtime(Arc::new(TokioAdapter::new()?));
    /// ```
    pub fn with_runtime<R>(mut self, rt: Arc<R>) -> Self
    where
        R: 'static + Send + Sync,
    {
        self.runtime = Some(rt);
        self
    }

    /// Configures a record type manually
    ///
    /// This is a low-level method for advanced use cases. Most users
    /// should use `register_record` instead.
    ///
    /// # Type Parameters
    /// * `T` - The record type
    ///
    /// # Arguments
    /// * `f` - A function that configures the record via `RecordRegistrar`
    ///
    /// # Returns
    /// `&mut Self` for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.configure::<SensorData>(|reg| {
    ///     reg.producer(|em, data| async move {
    ///         println!("Sensor: {:?}", data);
    ///     })
    ///     .consumer(|em, data| async move {
    ///         println!("Consumer: {:?}", data);
    ///     });
    /// });
    /// ```
    pub fn configure<T>(
        &mut self,
        f: impl for<'a> FnOnce(&'a mut RecordRegistrar<'a, T>),
    ) -> &mut Self
    where
        T: Send + 'static + Debug + Clone,
    {
        let entry = self
            .records
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(TypedRecord::<T>::new()));

        let rec = entry
            .as_typed_mut::<T>()
            .expect("type mismatch in record registry");

        let mut reg = RecordRegistrar { rec };
        f(&mut reg);
        self
    }

    /// Registers a self-registering record type
    ///
    /// This is the primary method for adding records to the database.
    /// The record type must implement `RecordT`.
    ///
    /// # Type Parameters
    /// * `R` - The record type implementing `RecordT`
    ///
    /// # Arguments
    /// * `cfg` - Configuration for the record
    ///
    /// # Returns
    /// `&mut Self` for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let sensor_cfg = SensorConfig { threshold: 100.0 };
    /// builder.register_record::<SensorData>(&sensor_cfg);
    /// ```
    pub fn register_record<R>(&mut self, cfg: &R::Config) -> &mut Self
    where
        R: RecordT,
    {
        self.configure::<R>(|reg| R::register(reg, cfg))
    }

    /// Builds the database
    ///
    /// Validates that all records have proper producer/consumer setup
    /// and constructs the final `AimDb` instance.
    ///
    /// # Returns
    /// `Ok(AimDb)` if successful, `Err` if validation fails or runtime not set
    ///
    /// # Errors
    /// - Runtime not set (use `with_runtime`)
    /// - Record validation failed (missing producer or consumers)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = builder.build()?;
    /// ```
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
            outboxes: Arc::new(Mutex::new(BTreeMap::new())),
        });

        Ok(AimDb { inner, runtime })
    }
}

impl Default for AimDbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer-consumer database
///
/// A database instance with type-safe record registration and
/// cross-record communication via the Emitter pattern.
///
/// # Design Philosophy
///
/// - **Type Safety**: Records identified by type, not strings
/// - **Reactive**: Data flows through producer-consumer pipelines
/// - **Observable**: Built-in call tracking and statistics
/// - **Runtime Agnostic**: Works with any Runtime implementation
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::experimental::AimDb;
///
/// // Build database
/// let db = AimDb::build_with(runtime, |b| {
///     b.register_record::<SensorData>(&sensor_cfg);
///     b.register_record::<Alert>(&alert_cfg);
/// })?;
///
/// // Produce data - flows through pipeline
/// db.produce(SensorData { temp: 75.0 }).await?;
///
/// // Inspect statistics
/// if let Some((calls, last)) = db.producer_stats::<SensorData>() {
///     println!("Producer called {} times", calls);
/// }
/// ```
pub struct AimDb {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter (type-erased)
    runtime: Arc<dyn core::any::Any + Send + Sync>,
}

impl AimDb {
    /// Internal accessor for the inner state
    ///
    /// This is used by adapter crates for advanced operations.
    /// Should not be used by application code.
    #[doc(hidden)]
    pub fn inner(&self) -> &Arc<AimDbInner> {
        &self.inner
    }

    /// Builds a database with a closure-based builder pattern
    ///
    /// This is the primary construction method for most use cases.
    ///
    /// # Arguments
    /// * `rt` - The runtime adapter to use
    /// * `f` - A closure that configures the builder
    ///
    /// # Returns
    /// `Ok(AimDb)` if successful, `Err` if validation fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = AimDb::build_with(runtime, |b| {
    ///     b.register_record::<SensorData>(&sensor_cfg);
    ///     b.register_record::<Alert>(&alert_cfg);
    /// })?;
    /// ```
    pub fn build_with<R>(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder)) -> DbResult<Self>
    where
        R: 'static + Send + Sync,
    {
        let mut b = AimDbBuilder::new().with_runtime(rt);
        f(&mut b);
        b.build()
    }

    /// Returns an emitter for cross-record communication
    ///
    /// The emitter can be cloned and passed to async tasks.
    ///
    /// # Returns
    /// An `Emitter` instance
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let emitter = db.emitter();
    /// emitter.emit(Alert::new("Test")).await?;
    /// ```
    pub fn emitter(&self) -> Emitter {
        // Clone the Arc to get a new reference
        let runtime_clone = self.runtime.clone();
        // Create emitter with the cloned runtime (already type-erased)
        Emitter {
            runtime: runtime_clone,
            inner: self.inner.clone(),
        }
    }

    /// Produces a value for a record type
    ///
    /// This triggers the producer and all consumers for the given type.
    ///
    /// # Type Parameters
    /// * `T` - The record type
    ///
    /// # Arguments
    /// * `value` - The value to produce
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err` if record type not registered
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// db.produce(SensorData { temp: 23.5 }).await?;
    /// ```
    pub async fn produce<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        self.emitter().emit::<T>(value).await
    }

    /// Returns producer statistics for a record type
    ///
    /// # Type Parameters
    /// * `T` - The record type
    ///
    /// # Returns
    /// `Some((calls, last_value))` if record exists and has a producer, `None` otherwise
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some((calls, last)) = db.producer_stats::<SensorData>() {
    ///     println!("Producer called {} times, last value: {:?}", calls, last);
    /// }
    /// ```
    pub fn producer_stats<T>(&self) -> Option<(u64, Option<T>)>
    where
        T: Send + 'static + Debug + Clone,
    {
        let rec = self.inner.records.get(&TypeId::of::<T>())?;
        let rec_typed = rec.as_typed::<T>()?;
        let stats = rec_typed.producer_stats()?;
        Some((stats.calls(), stats.last_arg()))
    }

    /// Returns consumer statistics for a record type
    ///
    /// # Type Parameters
    /// * `T` - The record type
    ///
    /// # Returns
    /// A vector of `(calls, last_value)` tuples, one per consumer
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// for (i, (calls, last)) in db.consumer_stats::<SensorData>().into_iter().enumerate() {
    ///     println!("Consumer {} called {} times", i, calls);
    /// }
    /// ```
    pub fn consumer_stats<T>(&self) -> Vec<(u64, Option<T>)>
    where
        T: Send + 'static + Debug + Clone,
    {
        let Some(rec) = self.inner.records.get(&TypeId::of::<T>()) else {
            return Vec::new();
        };
        let Some(rec_typed) = rec.as_typed::<T>() else {
            return Vec::new();
        };
        rec_typed
            .consumer_stats()
            .into_iter()
            .map(|s| (s.calls(), s.last_arg()))
            .collect()
    }

    /// Returns a reference to the runtime (downcasted)
    ///
    /// # Type Parameters
    /// * `R` - The expected runtime type
    ///
    /// # Returns
    /// `Some(&R)` if the runtime type matches, `None` otherwise
    pub fn runtime<R: 'static>(&self) -> Option<&R> {
        self.runtime.downcast_ref::<R>()
    }

    /// Initializes an MPSC outbox for external system communication
    ///
    /// This method creates a bounded MPSC channel for enqueueing messages to
    /// an external system worker (MQTT, Kafka, DDS, etc.). The sender is stored
    /// in the outbox registry for multi-producer access and the receiver is
    /// passed to the worker task.
    ///
    /// # Type Parameters
    ///
    /// * `R` - The runtime adapter type (must implement OutboxRuntimeSupport)
    /// * `T` - The payload type (must be `Send + 'static`)
    /// * `W` - The worker implementation that consumes messages
    ///
    /// # Arguments
    ///
    /// * `runtime` - The runtime adapter instance (for channel creation and spawning)
    /// * `config` - Configuration for the outbox (capacity, overflow behavior)
    /// * `worker` - The worker instance that will consume messages
    ///
    /// # Returns
    ///
    /// * `Ok(WorkerHandle)` - Handle for monitoring the worker task
    /// * `Err(DbError::OutboxAlreadyExists)` - If outbox for type `T` already exists
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::outbox::{OutboxConfig, OverflowBehavior, SinkWorker};
    /// use aimdb_tokio_adapter::TokioAdapter;
    ///
    /// // Define message type
    /// #[derive(Clone, Debug)]
    /// struct MqttMsg {
    ///     topic: String,
    ///     payload: Vec<u8>,
    /// }
    ///
    /// // Define worker
    /// struct MqttWorker { /* ... */ }
    /// impl SinkWorker<MqttMsg> for MqttWorker {
    ///     fn spawn(self, rt: Arc<dyn RuntimeAdapter>, rx: Box<dyn Any + Send>) -> WorkerHandle {
    ///         // Implementation
    ///     }
    /// }
    ///
    /// let runtime = Arc::new(TokioAdapter::new()?);
    /// let db = AimDb::build_with(runtime.clone(), |b| { /* ... */ })?;
    ///
    /// // Initialize outbox
    /// let handle = db.init_outbox::<TokioAdapter, MqttMsg, _>(
    ///     runtime.clone(),
    ///     OutboxConfig {
    ///         capacity: 1024,
    ///         overflow: OverflowBehavior::Block,
    ///     },
    ///     MqttWorker::new("mqtt://broker"),
    /// )?;
    ///
    /// // Now can enqueue messages from any emitter
    /// db.emitter().enqueue(MqttMsg { /* ... */ }).await?;
    /// ```
    ///
    /// # Thread Safety
    ///
    /// This method acquires a lock on the outbox registry. Multiple threads
    /// can safely call `init_outbox` with different types concurrently, but
    /// attempting to initialize the same type twice will return an error.
    pub fn init_outbox<R, T, W>(
        &self,
        runtime: Arc<R>,
        config: crate::outbox::OutboxConfig,
        worker: W,
    ) -> DbResult<crate::outbox::WorkerHandle>
    where
        R: crate::outbox::OutboxRuntimeSupport,
        T: Send + 'static,
        W: crate::outbox::SinkWorker<T>,
    {
        use crate::DbError;
        use core::any::TypeId;

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Initializing outbox for type {} with capacity {}",
            core::any::type_name::<T>(),
            config.capacity
        );

        let type_id = TypeId::of::<T>();

        // Check if outbox already exists
        {
            let outboxes = self.inner.outboxes.lock();
            #[cfg(feature = "std")]
            let outboxes_ref = outboxes.as_ref().expect("Failed to lock outboxes");
            #[cfg(not(feature = "std"))]
            let outboxes_ref = &*outboxes;

            if outboxes_ref.contains_key(&type_id) {
                return Err(DbError::OutboxAlreadyExists {
                    #[cfg(feature = "std")]
                    type_name: core::any::type_name::<T>().to_string(),
                    #[cfg(not(feature = "std"))]
                    _type_name: (),
                });
            }
        }

        // Create type-erased channel via runtime adapter
        let (sender, receiver) = runtime.create_outbox_channel::<T>(config.capacity);

        // Store sender in registry
        {
            let mut outboxes = self.inner.outboxes.lock();
            #[cfg(feature = "std")]
            let outboxes_mut = outboxes.as_mut().expect("Failed to lock outboxes");
            #[cfg(not(feature = "std"))]
            let outboxes_mut = &mut *outboxes;

            outboxes_mut.insert(type_id, sender);
        }

        // Spawn worker task
        // Cast runtime to Arc<dyn RuntimeAdapter> for worker
        let runtime_dyn: Arc<dyn RuntimeAdapter> = runtime as Arc<dyn RuntimeAdapter>;
        let handle = worker.spawn(runtime_dyn, receiver);

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Outbox initialized for type {} with worker ID {}",
            core::any::type_name::<T>(),
            handle.task_id()
        );

        Ok(handle)
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
            reg.producer(|_em, _data| async {})
                .consumer(|_em, _data| async {});
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
            reg.producer(|_em, _data| async {})
                .consumer(|_em, _data| async {});
        });

        assert_eq!(builder.records.len(), 1);
    }
}
