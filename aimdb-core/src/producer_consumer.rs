//! Self-registering records with producer-consumer pattern
//!
//! Provides the `RecordT` trait for self-registering records and
//! the `RecordRegistrar` for fluent registration API.

use core::fmt::Debug;
use core::future::Future;

use crate::typed_record::TypedRecord;

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions
/// for a specific record type.
///
/// # Type Parameters
/// * `T` - The record type
///
/// # Example
///
/// ```rust,ignore
/// reg.producer(|emitter, data| async move {
///     println!("Producer: {:?}", data);
/// })
/// .consumer(|emitter, data| async move {
///     println!("Consumer 1: {:?}", data);
/// })
/// .consumer(|emitter, data| async move {
///     println!("Consumer 2: {:?}", data);
/// });
/// ```
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
    /// The producer is called first when data is produced for this record type.
    /// Only one producer can be registered per record type.
    ///
    /// # Arguments
    /// * `f` - An async function taking `(Emitter, T)` and returning `()`
    ///
    /// # Returns
    /// `&mut Self` for method chaining
    ///
    /// # Panics
    /// Panics if a producer is already registered
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.producer(|emitter, data| async move {
    ///     println!("[producer] Processing: {:?}", data);
    ///     
    ///     // Can emit to other record types
    ///     if data.should_alert() {
    ///         emitter.emit(Alert::from(data)).await?;
    ///     }
    /// });
    /// ```
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
    /// Consumers are called after the producer, in the order they are registered.
    /// Multiple consumers can be registered for the same record type.
    ///
    /// # Arguments
    /// * `f` - An async function taking `(Emitter, T)` and returning `()`
    ///
    /// # Returns
    /// `&mut Self` for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.consumer(|emitter, data| async move {
    ///     println!("[consumer] Received: {:?}", data);
    ///     
    ///     // Can perform side effects
    ///     log_to_database(&data).await?;
    /// })
    /// .consumer(|emitter, data| async move {
    ///     println!("[consumer 2] Also received: {:?}", data);
    ///     
    ///     // Multiple consumers get the same data
    ///     send_to_metrics(&data).await?;
    /// });
    /// ```
    pub fn consumer<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.add_consumer(f);
        self
    }
}

/// Self-registering record trait
///
/// Records implementing this trait can register their own producer and
/// consumer functions, encapsulating their behavior with their type.
///
/// # Type Parameters
/// * `Config` - The configuration type for this record
///
/// # Design Philosophy
///
/// - **Self-Contained**: Records define their own behavior
/// - **Type-Safe**: Configuration is strongly typed per record
/// - **Composable**: Records can emit to other records via `Emitter`
/// - **Testable**: Configuration can be mocked for testing
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::experimental::{RecordT, RecordRegistrar, Emitter};
///
/// #[derive(Clone, Debug)]
/// struct SensorData {
///     temp: f32,
/// }
///
/// struct SensorConfig {
///     alert_threshold: f32,
/// }
///
/// impl RecordT for SensorData {
///     type Config = SensorConfig;
///     
///     fn register(reg: &mut RecordRegistrar<Self>, cfg: &Self::Config) {
///         let threshold = cfg.alert_threshold;
///         
///         reg.producer(|emitter, data| async move {
///             println!("Sensor reading: {}", data.temp);
///         })
///         .consumer(move |emitter, data| async move {
///             if data.temp > threshold {
///                 let alert = Alert { message: "High temperature!".into() };
///                 let _ = emitter.emit(alert).await;
///             }
///         });
///     }
/// }
/// ```
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
