//! Type-safe Producer-Consumer API
//!
//! Provides the complete typed API for producer-consumer patterns including:
//! - `Producer<T, R>` - Type-safe value production
//! - `Consumer<T, R>` - Type-safe value consumption
//! - `RecordRegistrar` - Fluent record configuration API
//! - `RecordT` trait - Self-registering records
//!
//! # Producer Example
//!
//! ```rust,ignore
//! #[service]
//! async fn temperature_producer<R: Runtime>(
//!     ctx: RuntimeContext<R>,
//!     producer: Producer<Temperature, R>,
//! ) {
//!     loop {
//!         let temp = read_sensor().await;
//!         producer.produce(temp).await?;
//!         ctx.time().sleep(ctx.time().secs(1)).await;
//!     }
//! }
//! ```
//!
//! # Consumer Example
//!
//! ```rust,ignore
//! #[service]
//! async fn temperature_monitor<R: Runtime>(
//!     ctx: RuntimeContext<R>,
//!     consumer: Consumer<Temperature, R>,
//! ) {
//!     let mut rx = consumer.subscribe()?;
//!     while let Ok(temp) = rx.recv().await {
//!         ctx.log().info(&format!("Temp: {:.1}°C", temp.celsius));
//!     }
//! }
//! ```
//!
//! # Record Registration Example
//!
//! ```rust,ignore
//! builder.configure::<Temperature>(|reg| {
//!     reg.buffer(buffer)
//!        .source(|producer, ctx| temperature_service(ctx, producer))
//!        .tap(|consumer| temperature_logger(consumer))
//!        .link_to("mqtt://sensors/temp")
//!        .with_serializer(|t| serde_json::to_vec(t))
//!        .finish();
//! });
//! ```

use core::fmt::Debug;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;

extern crate alloc;
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::typed_record::TypedRecord;
use crate::{AimDb, DbResult};

// ============================================================================
// Producer - Type-safe value production
// ============================================================================

/// Type-safe producer for a specific record type
///
/// `Producer<T, R>` provides scoped access to produce values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this producer can emit
/// * `R` - The runtime adapter type (e.g., TokioAdapter, EmbassyAdapter)
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it produces
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
pub struct Producer<T, R: aimdb_executor::Spawn + 'static> {
    /// Reference to the database
    db: Arc<AimDb<R>>,
    /// Phantom data to bind the type parameter T
    _phantom: PhantomData<T>,
}

impl<T, R> Producer<T, R>
where
    T: Send + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Create a new producer (internal use only)
    pub(crate) fn new(db: Arc<AimDb<R>>) -> Self {
        Self {
            db,
            _phantom: PhantomData,
        }
    }

    /// Produce a value of type T
    ///
    /// This triggers the entire pipeline for this record type:
    /// 1. All tap observers are notified
    /// 2. All link connectors are triggered
    /// 3. Buffers are updated (if configured)
    pub async fn produce(&self, value: T) -> DbResult<()> {
        self.db.produce(value).await
    }
}

impl<T, R> Clone for Producer<T, R>
where
    R: aimdb_executor::Spawn + 'static,
{
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            _phantom: PhantomData,
        }
    }
}

unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Send for Producer<T, R> {}
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Sync for Producer<T, R> {}

// Implement ProducerTrait for type-erased routing
impl<T, R> crate::connector::ProducerTrait for Producer<T, R>
where
    T: Send + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    fn produce_any<'a>(
        &'a self,
        value: Box<dyn core::any::Any + Send>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // Downcast the Box<dyn Any> to Box<T>
            let value = value.downcast::<T>().map_err(|_| {
                format!(
                    "Failed to downcast value to type {}",
                    core::any::type_name::<T>()
                )
            })?;

            // Produce the value
            self.produce(*value)
                .await
                .map_err(|e| format!("Failed to produce value: {}", e))
        })
    }
}

// ============================================================================
// Consumer - Type-safe value consumption
// ============================================================================

/// Type-safe consumer for a specific record type
///
/// `Consumer<T, R>` provides scoped access to subscribe to values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this consumer can subscribe to
/// * `R` - The runtime adapter type (e.g., TokioAdapter, EmbassyAdapter)
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it consumes
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
#[derive(Clone)]
pub struct Consumer<T, R: aimdb_executor::Spawn + 'static> {
    /// Reference to the database
    db: Arc<AimDb<R>>,
    /// Phantom data to bind the type parameter T
    _phantom: PhantomData<T>,
}

impl<T, R> Consumer<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Create a new consumer
    pub(crate) fn new(db: Arc<AimDb<R>>) -> Self {
        Self {
            db,
            _phantom: PhantomData,
        }
    }

    /// Subscribe to updates for this record type
    ///
    /// Returns a reader that yields values when they are produced.
    pub fn subscribe(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>> {
        self.db.subscribe::<T>()
    }
}

unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Send for Consumer<T, R> {}
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Sync for Consumer<T, R> {}

// ============================================================================
// RecordRegistrar - Fluent registration API
// ============================================================================

/// Type alias for typed serializer callbacks
type TypedSerializerFn<T> =
    Arc<dyn Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static>;

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions.
pub struct RecordRegistrar<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T, R>,
    /// Connector builders indexed by scheme
    pub(crate) connector_builders: &'a [Box<dyn crate::connector::ConnectorBuilder<R>>],
}

impl<'a, T, R> RecordRegistrar<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Registers a producer service for this record type (low-level API)
    ///
    /// **Note:** This is the foundational API used by runtime adapter implementations.
    /// Most users should use the higher-level `source()` method provided by runtime
    /// adapter extension traits (e.g., `TokioRecordRegistrarExt::source()`) which
    /// automatically extract the typed `RuntimeContext`.
    ///
    /// This method accepts the raw runtime context as `Arc<dyn Any>` and is used by:
    /// - Runtime adapter implementations to provide convenient wrappers
    /// - Internal connector implementations
    /// - Advanced use cases requiring direct control
    pub fn source_raw<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: FnOnce(crate::Producer<T, R>, Arc<dyn core::any::Any + Send + Sync>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.set_producer_service(f);
        self
    }

    /// Register a side-effect observer that taps into the data stream (low-level API)
    ///
    /// **Note:** This is the foundational API used by runtime adapter and connector implementations.
    /// Most users should use the higher-level `tap()` method provided by runtime
    /// adapter extension traits (e.g., `TokioRecordRegistrarExt::tap()`) which
    /// automatically extract the typed `RuntimeContext`.
    ///
    /// This method accepts the raw runtime context as `Arc<dyn Any>` and is used by:
    /// - Runtime adapter implementations to provide convenient wrappers
    /// - Internal connector implementations (e.g., `.link()` creates consumers via this method)
    /// - Advanced use cases requiring direct control
    pub fn tap_raw<F, Fut>(&'a mut self, f: F) -> &'a mut Self
    where
        F: FnOnce(crate::Consumer<T, R>, Arc<dyn core::any::Any + Send + Sync>) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        T: Sync,
    {
        self.rec.add_consumer(f);
        self
    }

    /// Configures a buffer for this record (low-level API)
    ///
    /// **Note:** This is the foundational API used by runtime adapter implementations.
    /// Most users should use the higher-level `buffer()` method provided by runtime
    /// adapter extension traits (e.g., `TokioRecordRegistrarExt::buffer()`) which
    /// accept `BufferCfg` and construct the appropriate buffer type automatically.
    ///
    /// This method accepts a boxed buffer trait object and is used by:
    /// - Runtime adapter implementations to provide convenient wrappers
    /// - Advanced use cases requiring custom buffer implementations
    ///
    /// **Note:** For metadata tracking in std mode, call `buffer_with_cfg()` instead,
    /// or call `buffer_cfg()` separately to set the configuration.
    pub fn buffer_raw(&'a mut self, buffer: Box<dyn crate::buffer::DynBuffer<T>>) -> &'a mut Self {
        self.rec.set_buffer(buffer);
        self
    }

    /// Configures a buffer with metadata tracking (std only)
    #[cfg(feature = "std")]
    pub fn buffer_with_cfg(
        &'a mut self,
        buffer: Box<dyn crate::buffer::DynBuffer<T>>,
        cfg: crate::buffer::BufferCfg,
    ) -> &'a mut Self {
        self.rec.set_buffer(buffer);
        self.rec.set_buffer_cfg(cfg);
        self
    }

    /// Sets the buffer configuration for metadata tracking (std only)
    #[cfg(feature = "std")]
    pub fn buffer_cfg(&'a mut self, cfg: crate::buffer::BufferCfg) -> &'a mut Self {
        self.rec.set_buffer_cfg(cfg);
        self
    }

    /// Enables JSON serialization for remote access (std only)
    ///
    /// Configures this record to support the `record.get` protocol method.
    /// Requires `T: serde::Serialize`.
    ///
    /// # Example
    /// ```rust,ignore
    /// builder.configure::<Temperature>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .with_serialization();  // Enable remote queries
    /// });
    /// ```
    #[cfg(feature = "std")]
    pub fn with_serialization(&'a mut self) -> &'a mut Self
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        self.rec.with_serialization();
        self
    }

    /// Adds a connector link for external system integration (DEPRECATED)
    ///
    /// **Deprecated**: Use `.link_to()` for outbound connectors or `.link_from()` for inbound.
    /// This method will be removed in a future version.
    #[deprecated(since = "0.2.0", note = "Use link_to() or link_from() instead")]
    pub fn link(&'a mut self, url: &str) -> OutboundConnectorBuilder<'a, T, R> {
        OutboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            serializer: None,
        }
    }

    /// Link TO external system (outbound: AimDB → External)
    ///
    /// Subscribes to buffer updates and publishes them to an external system.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.configure::<Temperature>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .link_to("mqtt://broker/sensors/temp")
    ///            .with_serializer(|t| serde_json::to_vec(t).unwrap())
    ///            .finish()
    /// });
    /// ```
    pub fn link_to(&'a mut self, url: &str) -> OutboundConnectorBuilder<'a, T, R> {
        OutboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            serializer: None,
        }
    }

    /// Link FROM external system (inbound: External → AimDB)
    ///
    /// Subscribes to an external data source and produces values into this record's buffer.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.configure::<LightState>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .link_from("mqtt://broker/lights/+/state")
    ///            .with_deserializer(|bytes| parse_light_state(bytes))
    ///            .finish()
    /// });
    /// ```
    pub fn link_from(&'a mut self, url: &str) -> InboundConnectorBuilder<'a, T, R> {
        InboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            deserializer: None,
        }
    }
}

// ============================================================================
// OutboundConnectorBuilder - Fluent outbound connector configuration
// ============================================================================

/// Builder for configuring outbound connector links (AimDB → External)
pub struct OutboundConnectorBuilder<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
> {
    registrar: &'a mut RecordRegistrar<'a, T, R>,
    url: String,
    config: Vec<(String, String)>,
    serializer: Option<TypedSerializerFn<T>>,
}

impl<'a, T, R> OutboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Adds a configuration option to the connector
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Sets a serialization callback
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static,
    {
        self.serializer = Some(Arc::new(f));
        self
    }

    /// Sets the MQTT Quality of Service level
    pub fn with_qos(mut self, qos: u8) -> Self {
        self.config.push(("qos".to_string(), qos.to_string()));
        self
    }

    /// Sets the MQTT retain flag
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.config.push(("retain".to_string(), retain.to_string()));
        self
    }

    /// Sets the publish timeout in milliseconds
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Finalizes the connector registration
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T, R> {
        use crate::connector::{ConnectorLink, ConnectorUrl};

        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        let mut link = ConnectorLink::new(url.clone());
        link.config = self.config.clone();

        if let Some(typed_callback) = self.serializer.clone() {
            let erased: crate::connector::SerializerFn =
                Arc::new(move |any: &dyn core::any::Any| {
                    if let Some(value) = any.downcast_ref::<T>() {
                        (typed_callback)(value)
                    } else {
                        Err(crate::connector::SerializeError::TypeMismatch)
                    }
                });
            link.serializer = Some(erased);
        }

        // Validation: Check that connector builder is registered
        let has_connector = self
            .registrar
            .connector_builders
            .iter()
            .any(|b| b.scheme() == scheme);

        if !has_connector {
            panic!(
                "No connector registered for scheme '{}'. Register via .with_connector() for {}",
                scheme, url_string
            );
        }

        // Validation: Serializer must be provided
        if link.serializer.is_none() {
            panic!(
                "Outbound connector requires a serializer. Call .with_serializer() for {}",
                url_string
            );
        }

        // Store the connector link - consumers will be created later in build()
        // after connectors are actually built
        self.registrar.rec.add_connector(link);
        self.registrar
    }
}

// ============================================================================
// InboundConnectorBuilder - Fluent inbound connector configuration
// ============================================================================

/// Type alias for typed deserializer callbacks
type TypedDeserializerFn<T> = Arc<dyn Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static>;

/// Builder for configuring inbound connector links (External → AimDB)
pub struct InboundConnectorBuilder<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
> {
    registrar: &'a mut RecordRegistrar<'a, T, R>,
    url: String,
    config: Vec<(String, String)>,
    deserializer: Option<TypedDeserializerFn<T>>,
}

impl<'a, T, R> InboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Adds a configuration option to the connector
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Sets a deserialization callback
    ///
    /// The deserializer takes raw bytes from the external system and converts
    /// them to the typed value `T`. Returns `Err(String)` if deserialization fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .link_from("mqtt://broker/sensors/temp")
    ///     .with_deserializer(|bytes| {
    ///         serde_json::from_slice::<Temperature>(bytes)
    ///             .map_err(|e| e.to_string())
    ///     })
    /// ```
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        self.deserializer = Some(Arc::new(f));
        self
    }

    /// Sets the MQTT Quality of Service level
    pub fn with_qos(mut self, qos: u8) -> Self {
        self.config.push(("qos".to_string(), qos.to_string()));
        self
    }

    /// Sets the publish timeout in milliseconds
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Finalizes the inbound connector registration
    ///
    /// # Panics
    ///
    /// - If no buffer is configured (inbound connectors require a buffer)
    /// - If no deserializer is provided
    /// - If no connector is registered for the URL scheme
    /// - If the URL is invalid
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T, R> {
        use crate::connector::{ConnectorUrl, InboundConnectorLink};

        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));

        let scheme = url.scheme().to_string();

        // Validation: Buffer must exist for inbound connectors
        if !self.registrar.rec.has_buffer() {
            panic!(
                "Inbound connector requires a buffer. Call .buffer() before .link_from() for record type {}",
                core::any::type_name::<T>()
            );
        }

        // Validation: Deserializer must be provided
        let Some(deserializer) = self.deserializer else {
            panic!(
                "Inbound connector requires a deserializer. Call .with_deserializer() for {}",
                self.url
            );
        };

        // Validation: Connector builder must be registered
        let has_connector = self
            .registrar
            .connector_builders
            .iter()
            .any(|b| b.scheme() == scheme);

        if !has_connector {
            panic!(
                "No connector registered for scheme '{}'. Register via .with_connector() for {}",
                scheme, self.url
            );
        }

        // Create type-erased deserializer
        let erased_deserializer: crate::connector::DeserializerFn =
            Arc::new(move |bytes: &[u8]| {
                deserializer(bytes).map(|val| Box::new(val) as Box<dyn core::any::Any + Send>)
            });

        // Create inbound connector link
        let mut link = InboundConnectorLink::new(url, erased_deserializer);
        link.config = self.config;

        // Add producer factory callback that captures type T (alloc feature)
        #[cfg(feature = "alloc")]
        {
            link = link.with_producer_factory(|db_any| {
                // Downcast Arc<dyn Any> to Arc<AimDb<R>>
                let db = db_any
                    .downcast::<crate::builder::AimDb<R>>()
                    .expect("Failed to downcast to AimDb");
                Box::new(Producer::<T, R>::new(db)) as Box<dyn crate::connector::ProducerTrait>
            });
        }

        // Add to record
        self.registrar.rec.add_inbound_connector(link);
        self.registrar
    }
}

// ============================================================================
// RecordT - Self-registering record trait
// ============================================================================

/// Self-registering record trait
///
/// Records implementing this trait register their producer and consumer
/// functions, encapsulating behavior with their type.
pub trait RecordT<R: aimdb_executor::Spawn + 'static>:
    Send + Sync + 'static + Debug + Clone
{
    /// Configuration type for this record
    type Config;

    /// Registers producer and consumer functions
    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self, R>, cfg: &Self::Config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    struct TestRecord {
        value: i32,
    }

    #[test]
    fn test_destination_extraction_simple() {
        use crate::connector::ConnectorUrl;
        let url = ConnectorUrl::parse("mqtt://sensors").unwrap();
        assert_eq!(url.host, "sensors");
        assert_eq!(url.path, None);
    }

    #[test]
    fn test_destination_extraction_multi_level() {
        use crate::connector::ConnectorUrl;
        let url = ConnectorUrl::parse("mqtt://sensors/temperature").unwrap();
        assert_eq!(url.host, "sensors");
        assert_eq!(url.path, Some("/temperature".to_string()));
    }

    #[test]
    fn test_destination_extraction_deep() {
        use crate::connector::ConnectorUrl;
        let url = ConnectorUrl::parse("mqtt://factory/floor1/sensors/temp").unwrap();
        assert_eq!(url.host, "factory");
        assert_eq!(url.path, Some("/floor1/sensors/temp".to_string()));
    }
}
