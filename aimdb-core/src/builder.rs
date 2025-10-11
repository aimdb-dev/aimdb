//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(not(feature = "std"))]
use alloc::collections::BTreeMap;

#[cfg(feature = "std")]
use std::collections::HashMap;

use crate::emitter::Emitter;
use crate::producer_consumer::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::DbResult;

/// Internal database state
///
/// Holds the registry of typed records, indexed by `TypeId`.
pub struct AimDbInner {
    /// Map from TypeId to type-erased records
    #[cfg(feature = "std")]
    pub(crate) records: HashMap<TypeId, Box<dyn AnyRecord>>,

    #[cfg(not(feature = "std"))]
    pub(crate) records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
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
    #[cfg(feature = "std")]
    records: HashMap<TypeId, Box<dyn AnyRecord>>,

    #[cfg(not(feature = "std"))]
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
            #[cfg(feature = "std")]
            records: HashMap::new(),
            #[cfg(not(feature = "std"))]
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
