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

use crate::emitter::Emitter;
use crate::producer_consumer::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::DbResult;

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
}

impl AimDbBuilder {
    /// Creates a new database builder
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            runtime: None,
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

        let mut reg = RecordRegistrar { rec };
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
