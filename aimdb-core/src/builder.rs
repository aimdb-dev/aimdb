//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;

extern crate alloc;

use alloc::collections::BTreeMap;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, string::String, sync::Arc, vec::Vec};

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

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
pub struct AimDbBuilder {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter (type-erased for storage)
    runtime: Option<Arc<dyn core::any::Any + Send + Sync>>,

    /// Connector pools indexed by scheme (mqtt, shmem, kafka, etc.)
    /// Used by .link() to route publishing requests to the correct protocol handler
    pub(crate) connector_pools: BTreeMap<String, Arc<dyn crate::pool::ConnectorPool>>,
}

impl AimDbBuilder {
    /// Creates a new database builder
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
            connector_pools: BTreeMap::new(),
        }
    }

    /// Sets the runtime adapter
    pub fn with_runtime<R>(mut self, rt: Arc<R>) -> Self
    where
        R: 'static + Send + Sync,
    {
        self.runtime = Some(rt);
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
        T: Send + 'static + Debug + Clone,
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
/// A database instance with type-safe record registration and cross-record
/// communication via the Emitter pattern. See `examples/` for usage.
pub struct AimDb {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter (type-erased)
    runtime: Arc<dyn core::any::Any + Send + Sync>,
}

impl Clone for AimDb {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
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
        R: 'static + Send + Sync,
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

    /// Returns producer statistics for a record type
    ///
    /// Returns `Some((calls, last_value))` if record exists, `None` otherwise.
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
    /// Returns a vector of `(calls, last_value)` tuples, one per consumer.
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
            reg.source(|_em, _data| async {}).tap(|_em, _data| async {});
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
            reg.source(|_em, _data| async {}).tap(|_em, _data| async {});
        });

        assert_eq!(builder.records.len(), 1);
    }
}
