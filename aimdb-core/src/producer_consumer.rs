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
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::typed_record::TypedRecord;

/// Type alias for typed serializer callbacks (reduces type complexity)
type TypedSerializerFn<T> =
    Arc<dyn Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static>;

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions.
pub struct RecordRegistrar<'a, T: Send + Sync + 'static + Debug + Clone> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T>,

    /// Connector pools indexed by scheme (mqtt, shmem, kafka, etc.)
    /// Used by .link() to route publishing requests to the correct protocol handler
    pub(crate) connector_pools: &'a BTreeMap<String, Arc<dyn crate::pool::ConnectorPool>>,
}

impl<'a, T> RecordRegistrar<'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Registers a producer service for this record type.
    ///
    /// The producer service is a long-running task that generates data and calls
    /// `producer.produce()` to emit values. It will be automatically spawned during `build()`.
    ///
    /// # Arguments
    /// * `f` - A function taking `(Producer<T>, RuntimeContext)` and returning a Future
    ///
    /// # Panics
    /// Panics if a source is already registered (each record can have only one source)
    ///
    /// # Example
    ///
    /// ```ignore
    /// reg.source(|producer, ctx| async move {
    ///     loop {
    ///         let temp = Temperature {
    ///             celsius: read_sensor(),
    ///             timestamp: timestamp(),
    ///         };
    ///         producer.produce(temp).await?;
    ///         ctx.time().sleep(ctx.time().secs(1)).await;
    ///     }
    /// });
    /// ```
    pub fn source<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: FnOnce(crate::Producer<T>, Arc<dyn core::any::Any + Send + Sync>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.set_producer_service(f);
        self
    }

    /// Register a side-effect observer that taps into the data stream.
    ///
    /// This is a non-consuming observer that gets called whenever `db.produce()` is invoked
    /// for this record type. Multiple `.tap()` calls can be chained to register multiple observers.
    ///
    /// # Use Cases
    /// - Logging and debugging
    /// - Metrics collection  
    /// - Validation and alerting
    /// - Audit trails
    ///
    /// # Example
    ///
    /// ```ignore
    /// reg.tap(|consumer| async move {
    ///     let mut reader = consumer.subscribe().unwrap();
    ///     while let Ok(temp) = reader.recv().await {
    ///         info!("Temperature: {:.1}°C", temp.celsius);
    ///     }
    /// })
    /// .tap(|consumer| async move {
    ///     let mut reader = consumer.subscribe().unwrap();
    ///     while let Ok(temp) = reader.recv().await {
    ///         metrics::gauge!("temperature", temp.celsius);
    ///     }
    /// });
    /// ```
    pub fn tap<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: FnOnce(crate::Consumer<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        T: Sync, // Required for Consumer::subscribe()
    {
        // Add as consumer - will be spawned as background task
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
    /// The URL scheme (mqtt://, shmem://, kafka://, etc.) routes to the appropriate
    /// connector pool registered via `.with_connector_pool()`.
    ///
    /// # URL Format
    ///
    /// `scheme://destination`
    ///
    /// Where:
    /// - `scheme` - Protocol type (mqtt, shmem, kafka, http, dds, etc.)
    /// - `destination` - Protocol-specific path (topic, segment, endpoint)
    ///
    /// # Arguments
    /// * `url` - The connector URL (e.g., "mqtt://sensors/temperature", "shmem://temp_data")
    ///
    /// # Returns
    /// A `ConnectorBuilder` for chaining configuration
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.link("mqtt://sensors/temperature")     // MQTT topic
    ///    .with_qos(1)
    ///    .with_retain(true)
    ///    .finish()
    ///    .link("shmem://temp_readings")          // Shared memory
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
pub struct ConnectorBuilder<'a, T: Send + Sync + 'static + Debug + Clone> {
    registrar: &'a mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    serializer: Option<TypedSerializerFn<T>>,
}

impl<'a, T> ConnectorBuilder<'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
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
    /// use aimdb_core::connector::SerializeError;
    ///
    /// reg.link("mqtt://broker:1883/topic")
    ///    .with_serializer(|temp: &Temperature| {
    ///        serde_json::to_vec(temp)
    ///            .map_err(|_| SerializeError::InvalidData)
    ///    })
    ///    .finish();
    /// ```
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static,
    {
        self.serializer = Some(Arc::new(f));
        self
    }

    /// Sets the MQTT Quality of Service level
    ///
    /// # Arguments
    /// * `qos` - QoS level (0, 1, or 2)
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_qos(mut self, qos: u8) -> Self {
        self.config.push(("qos".to_string(), qos.to_string()));
        self
    }

    /// Sets the MQTT retain flag
    ///
    /// # Arguments
    /// * `retain` - Whether to retain messages on the broker
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.config.push(("retain".to_string(), retain.to_string()));
        self
    }

    /// Sets the publish timeout in milliseconds
    ///
    /// Primarily used in Embassy environments where explicit timeouts are required.
    ///
    /// # Arguments
    /// * `timeout_ms` - Timeout in milliseconds
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Finalizes the connector registration
    ///
    /// Parses the URL, looks up the connector pool by scheme, and registers
    /// a consumer that publishes via the appropriate protocol handler.
    ///
    /// # URL Parsing
    ///
    /// The URL is parsed as `scheme://destination`:
    /// - `scheme` - Routes to the connector pool (must be registered via `.with_connector_pool()`)
    /// - `destination` - Protocol-specific path (topic, segment, endpoint)
    ///
    /// # Auto-Registration
    ///
    /// If a serializer is provided and a pool exists for the scheme, this method
    /// automatically registers a consumer that:
    /// 1. Serializes each value using the provided callback
    /// 2. Publishes to the destination via the connector pool
    ///
    /// # Panics
    /// Panics if the URL is invalid
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.link("mqtt://sensors/temperature")
    ///    .with_serializer(|temp| serde_json::to_vec(temp))
    ///    .finish();  // Auto-registers MQTT consumer
    /// ```
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T> {
        use crate::connector::{ConnectorLink, ConnectorUrl};

        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        // Add the connector link (storing metadata)
        let mut link = ConnectorLink::new(url.clone());
        link.config = self.config.clone();

        // Convert typed serializer to type-erased version
        let serializer_opt = if let Some(typed_callback) = self.serializer.clone() {
            let erased: crate::connector::SerializerFn =
                Arc::new(move |any: &dyn core::any::Any| {
                    if let Some(value) = any.downcast_ref::<T>() {
                        (typed_callback)(value)
                    } else {
                        Err(crate::connector::SerializeError::TypeMismatch)
                    }
                });
            link.serializer = Some(erased);
            Some(self.serializer.clone().unwrap())
        } else {
            None
        };

        // Check if we have both a serializer and a pool
        let has_serializer = serializer_opt.is_some();
        let has_pool = self.registrar.connector_pools.get(&scheme).is_some();

        // Auto-register consumer if pool is available for this scheme
        if let (Some(serializer), Some(pool)) =
            (serializer_opt, self.registrar.connector_pools.get(&scheme))
        {
            // Store Arc references for the closure
            let pool_clone = pool.clone();
            let url_clone = url_string.clone();
            let config_clone = self.config.clone();

            // Register a consumer that publishes via the connector pool
            let rec = &mut self.registrar.rec;
            rec.add_consumer(move |consumer| {
                let pool_ref = pool_clone.clone();
                let url_ref = url_clone.clone();
                let config_ref = config_clone.clone();
                let ser = serializer.clone();

                async move {
                    // Subscribe to buffer and forward each value to connector
                    let Ok(mut reader) = consumer.subscribe() else {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Failed to subscribe to buffer for connector {}", url_ref);
                        return;
                    };

                    while let Ok(value) = reader.recv().await {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "Connector triggered for {} with type {}",
                            url_ref,
                            core::any::type_name::<T>()
                        );

                        #[cfg(feature = "defmt")]
                        defmt::debug!("Connector triggered for {}", url_ref);

                        // Serialize the value
                        match ser(&value) {
                            Ok(bytes) => {
                                // Call the connector pool's publish method
                                if let Err(_e) =
                                    publish_via_pool(&pool_ref, &url_ref, &config_ref, bytes).await
                                {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!("Failed to publish: {:?}", _e);

                                    #[cfg(feature = "defmt")]
                                    defmt::error!("Publish failed: {:?}", _e);
                                }
                            }
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!("Failed to serialize: {:?}", _e);

                                #[cfg(feature = "defmt")]
                                defmt::error!("Serialization failed");
                            }
                        }
                    }
                }
            });
        } else if !has_serializer {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Connector configured for {} but no serializer provided. \
                Use .with_serializer() to enable automatic publishing.",
                url_string
            );
        } else if !has_pool {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Connector configured for {} but no pool registered for scheme '{}'. \
                Use builder.with_connector_pool(\"{}\", pool) to enable publishing.",
                url_string,
                scheme,
                scheme
            );

            #[cfg(feature = "defmt")]
            defmt::warn!(
                "No pool registered for scheme: {}. Use with_connector_pool().",
                scheme
            );
        }

        self.registrar.rec.add_connector(link);
        self.registrar
    }
}

/// Helper function to publish via a connector pool
///
/// Parses the configuration and calls the pool's publish method to handle
/// the actual MQTT/Kafka/etc. publishing.
///
/// # Arguments
/// * `pool` - The connector pool to use for publishing
/// * `url` - The connector URL (e.g., "mqtt://sensors/temp")
/// * `config` - Key-value configuration pairs (qos, retain, timeout_ms, etc.)
/// * `bytes` - The serialized message payload
async fn publish_via_pool(
    pool: &Arc<dyn crate::pool::ConnectorPool>,
    url: &str,
    config: &[(String, String)],
    bytes: Vec<u8>,
) -> Result<(), crate::pool::PublishError> {
    use crate::connector::ConnectorUrl;
    use crate::pool::ConnectorConfig;

    // Parse the URL to extract destination (topic/path/segment)
    let parsed_url =
        ConnectorUrl::parse(url).map_err(|_| crate::pool::PublishError::InvalidDestination)?;

    // Extract destination from URL
    // For pool-based connectors, the full topic is: host + path
    // e.g., "mqtt://sensors/temperature" -> host="sensors", path="/temperature"
    // The destination should be "sensors/temperature"
    let destination: String = if let Some(path) = parsed_url.path.as_deref() {
        let path_part = path.trim_start_matches('/');
        if path_part.is_empty() {
            // Just host, no path: mqtt://topic -> "topic"
            parsed_url.host.clone()
        } else {
            // Host + path: mqtt://sensors/temperature -> "sensors/temperature"
            alloc::format!("{}/{}", parsed_url.host, path_part)
        }
    } else {
        // No path: mqtt://topic -> "topic"
        parsed_url.host.clone()
    };

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Publishing to destination '{}' via {} pool (original URL: {})",
        destination,
        parsed_url.scheme,
        url
    );

    // Parse configuration into ConnectorConfig
    let mut connector_config = ConnectorConfig {
        qos: 0,
        retain: false,
        timeout_ms: Some(5000),
        protocol_options: Vec::new(),
    };

    for (key, value) in config {
        match key.as_str() {
            "qos" => {
                connector_config.qos = value.parse().unwrap_or(0).min(2);
            }
            "retain" => {
                connector_config.retain = value.parse().unwrap_or(false);
            }
            "timeout_ms" => {
                connector_config.timeout_ms = value.parse().ok();
            }
            _ => {
                // Store unknown keys in protocol_options for protocol-specific use
                connector_config
                    .protocol_options
                    .push((key.clone(), value.clone()));
            }
        }
    }

    // Call the pool's publish method
    pool.publish(&destination, &connector_config, &bytes).await
}

/// Self-registering record trait
///
/// Records implementing this trait register their producer and consumer
/// functions, encapsulating behavior with their type.
///
/// See `examples/producer-consumer-demo` for complete examples.
pub trait RecordT: Send + Sync + 'static + Debug + Clone {
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
    ///     // Set up data source
    ///     reg.source(|emitter, data| async move {
    ///         // Process incoming data
    ///         process(&data).await;
    ///     });
    ///     
    ///     // Set up observers
    ///     reg.tap(|emitter, data| async move {
    ///         // React to data
    ///         log(&data).await;
    ///     })
    ///     .tap(|emitter, data| async move {
    ///         // Collect metrics
    ///         metrics(&data).await;
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

            // Register producer service (would generate data in real usage)
            reg.source(|_producer, _ctx| async move {
                // Producer service would loop and call producer.produce()
                // For this test, we just register it
            })
            .tap(move |_consumer| async move {
                // Read from buffer and check threshold
                // In real usage, would subscribe and loop
                let _ = threshold; // Use the captured variable
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
                connector_pools: &BTreeMap::new(),
            };
            TestData::register(&mut reg, &cfg);
        }

        // Verify source and tap were added
        assert!(record.has_producer_service());
        assert_eq!(record.consumer_count(), 1);
    }

    #[test]
    fn test_record_validation() {
        use crate::typed_record::AnyRecord;

        let mut record = TypedRecord::<TestData>::new();
        let cfg = TestConfig { threshold: 50 };

        {
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pools: &BTreeMap::new(),
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
                connector_pools: &BTreeMap::new(),
            };

            // Register connectors using the new .link() API - chainable!
            reg.link("mqtt://sensors/temperature")
                .finish()
                .link("kafka://events/system")
                .finish();
        }

        // Verify connectors were registered
        assert_eq!(record.connector_count(), 2);

        let connectors = record.connectors();
        assert_eq!(connectors[0].url.scheme, "mqtt");
        assert_eq!(connectors[1].url.scheme, "kafka");
    }

    #[test]
    #[should_panic(expected = "Invalid connector URL")]
    fn test_connector_invalid_url() {
        let mut record = TypedRecord::<TestData>::new();

        {
            let mut reg = RecordRegistrar {
                rec: &mut record,
                connector_pools: &BTreeMap::new(),
            };

            // This should panic due to invalid URL
            let _ = reg.link("invalid-url-without-scheme").finish();
        }
    }

    #[test]
    fn test_destination_extraction_simple() {
        use crate::connector::ConnectorUrl;

        // Single-level topic: mqtt://sensors -> host="sensors", path=None
        let url = ConnectorUrl::parse("mqtt://sensors").unwrap();
        assert_eq!(url.host, "sensors");
        assert_eq!(url.path, None);
    }

    #[test]
    fn test_destination_extraction_multi_level() {
        use crate::connector::ConnectorUrl;

        // Multi-level topic: mqtt://sensors/temperature -> host="sensors", path="/temperature"
        let url = ConnectorUrl::parse("mqtt://sensors/temperature").unwrap();
        assert_eq!(url.host, "sensors");
        assert_eq!(url.path, Some("/temperature".to_string()));
    }

    #[test]
    fn test_destination_extraction_deep() {
        use crate::connector::ConnectorUrl;

        // Deep topic: mqtt://factory/floor1/sensors/temp
        let url = ConnectorUrl::parse("mqtt://factory/floor1/sensors/temp").unwrap();
        assert_eq!(url.host, "factory");
        assert_eq!(url.path, Some("/floor1/sensors/temp".to_string()));
    }
}
