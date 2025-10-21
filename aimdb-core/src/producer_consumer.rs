//! Self-registering records with producer-consumer pattern
//!
//! Provides the `RecordT` trait for self-registering records and
//! the `RecordRegistrar` for fluent registration API.
//!
//! See `examples/producer-consumer-demo` for usage examples.

use core::fmt::Debug;
use core::future::Future;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

#[cfg(feature = "std")]
use std::boxed::Box;

use crate::typed_record::TypedRecord;

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions.
pub struct RecordRegistrar<'a, T: Send + 'static + Debug + Clone> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T>,
}

impl<'a, T> RecordRegistrar<'a, T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Registers a producer function
    ///
    /// Called first when data is produced. Only one producer per record type.
    /// Panics if producer already registered.
    pub fn producer<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.set_producer(f);
        self
    }

    /// Registers a consumer function
    ///
    /// Multiple consumers can be registered. Each receives a copy of the data.
    pub fn consumer<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.add_consumer(f);
        self
    }

    /// Configures a buffer for this record
    ///
    /// When set, `produce()` enqueues to the buffer. A dispatcher task
    /// should drain the buffer and invoke producer/consumer functions.
    pub fn buffer(&'a mut self, buffer: Box<dyn crate::buffer::DynBuffer<T>>) -> &'a mut Self {
        self.rec.set_buffer(buffer);
        self
    }
}

/// Self-registering record trait
///
/// Records implementing this trait register their producer and consumer
/// functions, encapsulating behavior with their type.
///
/// See `examples/producer-consumer-demo` for complete examples.
pub trait RecordT: Send + 'static + Debug + Clone {
    /// Configuration type for this record
    ///
    /// This type is passed to the `register` method and can contain
    /// any configuration needed to set up producers and consumers.
    type Config;

    /// Registers producer and consumer functions
    ///
    /// This method is called during database construction to set up
    /// the data flow for this record type.
    ///
    /// # Arguments
    /// * `reg` - The registrar for adding producer/consumer functions
    /// * `cfg` - Configuration specific to this record type
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn register(reg: &mut RecordRegistrar<Self>, cfg: &Self::Config) {
    ///     // Set up producer
    ///     reg.producer(|emitter, data| async move {
    ///         // Process incoming data
    ///         process(&data).await;
    ///     });
    ///     
    ///     // Set up consumers
    ///     reg.consumer(|emitter, data| async move {
    ///         // React to data
    ///         react(&data).await;
    ///     });
    /// }
    /// ```
    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, cfg: &Self::Config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_record::TypedRecord;

    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    struct TestConfig {
        threshold: i32,
    }

    impl RecordT for TestData {
        type Config = TestConfig;

        fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, cfg: &Self::Config) {
            let threshold = cfg.threshold;

            reg.producer(|_emitter, data| async move {
                assert!(data.value >= 0);
            })
            .consumer(move |_emitter, data| async move {
                if data.value > threshold {
                    // Would emit alert in real usage
                }
            });
        }
    }

    #[test]
    fn test_record_registrar() {
        let mut record = TypedRecord::<TestData>::new();
        let cfg = TestConfig { threshold: 50 };

        {
            let mut reg = RecordRegistrar { rec: &mut record };
            TestData::register(&mut reg, &cfg);
        }

        // Verify producer and consumer were added
        assert!(record.producer_stats().is_some());
        assert_eq!(record.consumer_stats().len(), 1);
    }

    #[test]
    fn test_record_validation() {
        use crate::typed_record::AnyRecord;

        let mut record = TypedRecord::<TestData>::new();
        let cfg = TestConfig { threshold: 50 };

        {
            let mut reg = RecordRegistrar { rec: &mut record };
            TestData::register(&mut reg, &cfg);
        }

        // Should be valid after registration
        assert!(record.validate().is_ok());
    }
}
