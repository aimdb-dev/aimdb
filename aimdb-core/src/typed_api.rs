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
//!        .link("mqtt://sensors/temp")
//!        .with_serializer(|t| serde_json::to_vec(t))
//!        .finish();
//! });
//! ```

use core::fmt::Debug;
use core::future::Future;
use core::marker::PhantomData;

extern crate alloc;
use alloc::{
    boxed::Box,
    collections::BTreeMap,
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
    /// Create a new consumer (internal use only)
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
    /// Connectors indexed by scheme
    pub(crate) connectors: &'a BTreeMap<String, Arc<dyn crate::transport::Connector>>,
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
    pub fn buffer_raw(&'a mut self, buffer: Box<dyn crate::buffer::DynBuffer<T>>) -> &'a mut Self {
        self.rec.set_buffer(buffer);
        self
    }

    /// Adds a connector link for external system integration
    pub fn link(&'a mut self, url: &str) -> ConnectorBuilder<'a, T, R> {
        ConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            serializer: None,
        }
    }
}

// ============================================================================
// ConnectorBuilder - Fluent connector configuration
// ============================================================================

/// Builder for configuring connector links
pub struct ConnectorBuilder<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
> {
    registrar: &'a mut RecordRegistrar<'a, T, R>,
    url: String,
    config: Vec<(String, String)>,
    serializer: Option<TypedSerializerFn<T>>,
}

impl<'a, T, R> ConnectorBuilder<'a, T, R>
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

        let has_serializer = serializer_opt.is_some();
        let has_connector = self.registrar.connectors.get(&scheme).is_some();

        if let (Some(serializer), Some(connector)) =
            (serializer_opt, self.registrar.connectors.get(&scheme))
        {
            let connector_clone = connector.clone();
            let url_clone = url_string.clone();
            let config_clone = self.config.clone();

            let rec = &mut self.registrar.rec;
            rec.add_consumer(move |consumer, _ctx_any| {
                let connector_ref = connector_clone.clone();
                let url_ref = url_clone.clone();
                let config_ref = config_clone.clone();
                let ser = serializer.clone();

                async move {
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

                        match ser(&value) {
                            Ok(bytes) => {
                                if let Err(_e) = publish_via_connector(
                                    &connector_ref,
                                    &url_ref,
                                    &config_ref,
                                    bytes,
                                )
                                .await
                                {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!("Failed to publish: {:?}", _e);
                                }
                            }
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!("Failed to serialize: {:?}", _e);
                            }
                        }
                    }
                }
            });
        } else if !has_serializer {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Connector configured for {} but no serializer provided.",
                url_string
            );
        } else if !has_connector {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Connector configured for {} but no connector registered for scheme '{}'.",
                url_string,
                scheme
            );
        }

        self.registrar.rec.add_connector(link);
        self.registrar
    }
}

/// Helper function to publish via a connector
async fn publish_via_connector(
    connector: &Arc<dyn crate::transport::Connector>,
    url: &str,
    config: &[(String, String)],
    bytes: Vec<u8>,
) -> Result<(), crate::transport::PublishError> {
    use crate::connector::ConnectorUrl;
    use crate::transport::ConnectorConfig;

    let parsed_url =
        ConnectorUrl::parse(url).map_err(|_| crate::transport::PublishError::InvalidDestination)?;

    let destination: String = if let Some(path) = parsed_url.path.as_deref() {
        let path_part = path.trim_start_matches('/');
        if path_part.is_empty() {
            parsed_url.host.clone()
        } else {
            alloc::format!("{}/{}", parsed_url.host, path_part)
        }
    } else {
        parsed_url.host.clone()
    };

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
                connector_config
                    .protocol_options
                    .push((key.clone(), value.clone()));
            }
        }
    }

    connector
        .publish(&destination, &connector_config, &bytes)
        .await
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
