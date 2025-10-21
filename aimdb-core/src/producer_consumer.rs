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
use alloc::{boxed::Box, string::ToString};

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

    /// Adds a connector link for external system integration
    ///
    /// Creates a fluent builder chain for configuring protocol connectors.
    /// Supports MQTT, Kafka, HTTP, WebSocket, and custom protocols.
    ///
    /// # Arguments
    /// * `url` - The connector URL (e.g., "mqtt://broker:1883", "kafka://host:9092/topic")
    ///
    /// # Returns
    /// A `ConnectorBuilder` for chaining configuration
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.link("mqtt://broker.local:1883")
    ///    .with_config("client_id", "my-device")
    ///    .with_config("qos", "1")
    ///    .finish();
    /// ```
    pub fn link(&'a mut self, url: &str) -> ConnectorBuilder<'a, T> {
        ConnectorBuilder {
            registrar: self,
            url: url.to_string(),
        }
    }
}

/// Builder for configuring connector links
///
/// Provides a fluent API for adding connector configuration options
/// before finalizing the connector registration.
pub struct ConnectorBuilder<'a, T: Send + 'static + Debug + Clone> {
    registrar: &'a mut RecordRegistrar<'a, T>,
    #[cfg(feature = "std")]
    url: std::string::String,
    #[cfg(not(feature = "std"))]
    url: alloc::string::String,
}

impl<'a, T> ConnectorBuilder<'a, T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Adds a configuration option to the connector
    ///
    /// Configuration options are protocol-specific and passed to the
    /// connector implementation during initialization.
    ///
    /// # Arguments
    /// * `key` - Configuration key (e.g., "client_id", "qos", "timeout")
    /// * `value` - Configuration value
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_config(self, _key: &str, _value: &str) -> Self {
        // TODO: Store configuration options in a temporary structure
        // For now, we just return self for the fluent API
        self
    }

    /// Finalizes the connector registration
    ///
    /// Parses the URL and adds the connector link to the record.
    /// Returns the registrar for further chaining.
    ///
    /// # Panics
    /// Panics if the URL is invalid
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T> {
        use crate::connector::{ConnectorLink, ConnectorUrl};

        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));
        let link = ConnectorLink::new(url);
        self.registrar.rec.add_connector(link);
        self.registrar
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

    #[test]
    fn test_connector_registration() {
        let mut record = TypedRecord::<TestData>::new();

        {
            let mut reg = RecordRegistrar { rec: &mut record };

            // Register connectors using the new .link() API - chainable!
            reg.link("mqtt://broker.example.com:1883")
                .finish()
                .link("kafka://kafka1:9092/my-topic")
                .finish();
        }

        // Verify connectors were registered
        assert_eq!(record.connector_count(), 2);

        let connectors = record.connectors();
        assert_eq!(connectors[0].url.scheme, "mqtt");
        assert_eq!(connectors[0].url.host, "broker.example.com");
        assert_eq!(connectors[0].url.port, Some(1883));

        assert_eq!(connectors[1].url.scheme, "kafka");
        assert!(connectors[1].url.host.contains("kafka1"));
    }

    #[test]
    #[should_panic(expected = "Invalid connector URL")]
    fn test_connector_invalid_url() {
        let mut record = TypedRecord::<TestData>::new();

        {
            let mut reg = RecordRegistrar { rec: &mut record };

            // This should panic due to invalid URL
            let _ = reg.link("invalid-url-without-scheme").finish();
        }
    }
}
