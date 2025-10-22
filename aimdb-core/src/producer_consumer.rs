//! Self-registering records with producer-consumer pattern
//!
//! Provides the `RecordT` trait for self-registering records and
//! the `RecordRegistrar` for fluent registration API.
//!
//! See `examples/producer-consumer-demo` for usage examples.

use core::fmt::Debug;
use core::future::Future;

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::typed_record::TypedRecord;

/// Type alias for typed serializer callbacks (reduces type complexity)
type TypedSerializerFn<T> = Arc<dyn Fn(&T) -> Result<Vec<u8>, String> + Send + Sync + 'static>;

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions.
pub struct RecordRegistrar<'a, T: Send + 'static + Debug + Clone> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T>,

    /// Optional connector pool for MQTT, Kafka, etc.
    pub(crate) connector_pool: Option<Arc<dyn crate::pool::MqttConnectorPool>>,
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
            config: Vec::new(),
            serializer: None,
        }
    }
}

/// Builder for configuring connector links
///
/// Provides a fluent API for adding connector configuration options
/// before finalizing the connector registration.
pub struct ConnectorBuilder<'a, T: Send + 'static + Debug + Clone> {
    registrar: &'a mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    serializer: Option<TypedSerializerFn<T>>,
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
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Sets a serialization callback for converting record values to bytes
    ///
    /// The callback receives a reference to the record value and returns
    /// a `Result<Vec<u8>, String>` containing the serialized bytes or an error message.
    ///
    /// # Arguments
    /// * `f` - Serialization function that takes `&T` and returns `Result<Vec<u8>, String>`
    ///
    /// # Returns
    /// Self for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use serde_json;
    ///
    /// reg.link("mqtt://broker:1883/topic")
    ///    .with_serializer(|temp: &Temperature| {
    ///        serde_json::to_vec(temp).map_err(|e| e.to_string())
    ///    })
    ///    .finish();
    /// ```
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        self.serializer = Some(Arc::new(f));
        self
    }

    /// Finalizes the connector registration
    ///
    /// Parses the URL and adds the connector link to the record.
    /// Returns the registrar for further chaining.
    ///
    /// For MQTT connectors (mqtt:// or mqtts://), this automatically registers
    /// a consumer that subscribes to the buffer and publishes to the MQTT broker.
    ///
    /// Finalizes the connector registration
    ///
    /// Parses the URL and adds the connector link to the record.
    /// Returns the registrar for further chaining.
    ///
    /// For MQTT connectors (mqtt:// or mqtts://), if a connector pool is available
    /// and a serializer is provided, this automatically registers a consumer that
    /// publishes to the MQTT broker.
    ///
    /// # Panics
    /// Panics if the URL is invalid
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T> {
        use crate::connector::{ConnectorLink, ConnectorUrl};

        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        // Add the connector link (storing metadata)
        let mut link = ConnectorLink::new(url);
        link.config = self.config.clone();

        // Convert typed serializer to type-erased version
        let serializer_opt = if let Some(typed_callback) = self.serializer.clone() {
            let erased: crate::connector::SerializerFn =
                Arc::new(move |any: &dyn core::any::Any| {
                    if let Some(value) = any.downcast_ref::<T>() {
                        (typed_callback)(value)
                    } else {
                        Err(format!(
                            "Type mismatch in serializer: expected {}, got different type",
                            core::any::type_name::<T>()
                        ))
                    }
                });
            link.serializer = Some(erased);
            Some(self.serializer.clone().unwrap())
        } else {
            None
        };

        // Auto-register MQTT consumer if pool is available
        if let (Some(serializer), Some(pool)) = (serializer_opt, &self.registrar.connector_pool) {
            if scheme == "mqtt" || scheme == "mqtts" {
                // Store Arc references for the closure
                let pool_clone = pool.clone();
                let url_clone = url_string.clone();
                let config_clone = self.config.clone();

                // Register a consumer that publishes to MQTT
                let rec = &mut self.registrar.rec;
                rec.add_consumer(move |_em, value: T| {
                    let pool_ref = pool_clone.clone();
                    let url_ref = url_clone.clone();
                    let config_ref = config_clone.clone();
                    let ser = serializer.clone();

                    async move {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "MQTT consumer triggered for {} with type {}",
                            url_ref,
                            core::any::type_name::<T>()
                        );

                        // Serialize the value
                        match ser(&value) {
                            Ok(bytes) => {
                                // Call the connector pool's publish method
                                if let Err(_e) =
                                    publish_via_pool(&pool_ref, &url_ref, &config_ref, bytes).await
                                {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!("Failed to publish to MQTT: {}", _e);
                                }
                            }
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!("Failed to serialize for MQTT: {}", _e);
                            }
                        }
                    }
                });
            }
        } else if scheme == "mqtt" || scheme == "mqtts" {
            // MQTT URL but no pool or no serializer
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "MQTT connector configured for {} but no connector pool provided. \
                Use builder.with_connector_pool() to enable automatic MQTT publishing.",
                url_string
            );
        }

        self.registrar.rec.add_connector(link);
        self.registrar
    }
}

/// Helper function to publish via a connector pool
///
/// Calls the pool's publish method to handle the actual MQTT/Kafka/etc. publishing.
async fn publish_via_pool(
    pool: &Arc<dyn crate::pool::MqttConnectorPool>,
    url: &str,
    config: &[(String, String)],
    bytes: Vec<u8>,
) -> Result<(), String> {
    pool.publish(url, config, bytes).await
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
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pool: None,
            };
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
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pool: None,
            };
            TestData::register(&mut reg, &cfg);
        }

        // Should be valid after registration
        assert!(record.validate().is_ok());
    }

    #[test]
    fn test_connector_registration() {
        let mut record = TypedRecord::<TestData>::new();

        {
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pool: None,
            };

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
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pool: None,
            };

            // This should panic due to invalid URL
            let _ = reg.link("invalid-url-without-scheme").finish();
        }
    }
}
