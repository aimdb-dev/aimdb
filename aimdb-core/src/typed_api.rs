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
//! builder.configure::<Temperature>("sensors.outdoor", |reg| {
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
    /// Optional record key for key-based routing (enables multiple records of same type)
    #[cfg(feature = "alloc")]
    record_key: Option<String>,
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
            #[cfg(feature = "alloc")]
            record_key: None,
            _phantom: PhantomData,
        }
    }

    /// Create a new producer bound to a specific record key
    ///
    /// This enables multiple records of the same type by routing
    /// produce() calls to the specific record identified by the key.
    #[cfg(feature = "alloc")]
    pub(crate) fn from_key_bound(db: Arc<AimDb<R>>, key: String) -> Self {
        Self {
            db,
            record_key: Some(key),
            _phantom: PhantomData,
        }
    }

    /// Produce a value of type T
    ///
    /// This triggers the entire pipeline for this record type:
    /// 1. All tap observers are notified
    /// 2. All link connectors are triggered
    /// 3. Buffers are updated (if configured)
    ///
    /// If this producer was created with a record key, the value is
    /// routed to that specific record. Otherwise, uses type-based routing
    /// (which fails if multiple records of the same type exist).
    pub async fn produce(&self, value: T) -> DbResult<()> {
        #[cfg(feature = "alloc")]
        if let Some(key) = &self.record_key {
            return self.db.produce_by_key::<T>(key, value).await;
        }
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
            #[cfg(feature = "alloc")]
            record_key: self.record_key.clone(),
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
    /// Optional record key for key-based routing (enables multiple records of same type)
    #[cfg(feature = "alloc")]
    record_key: Option<String>,
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
            #[cfg(feature = "alloc")]
            record_key: None,
            _phantom: PhantomData,
        }
    }

    /// Create a new consumer bound to a specific record key
    ///
    /// This enables multiple records of the same type by routing
    /// subscribe() calls to the specific record identified by the key.
    #[cfg(feature = "alloc")]
    pub(crate) fn from_key_bound(db: Arc<AimDb<R>>, key: String) -> Self {
        Self {
            db,
            record_key: Some(key),
            _phantom: PhantomData,
        }
    }

    /// Subscribe to updates for this record type
    ///
    /// Returns a reader that yields values when they are produced.
    /// If this consumer was created with a record key, subscribes to
    /// that specific record. Otherwise, uses type-based routing.
    pub fn subscribe(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>> {
        #[cfg(feature = "alloc")]
        if let Some(key) = &self.record_key {
            return self.db.subscribe_by_key::<T>(key);
        }
        self.db.subscribe::<T>()
    }
}

unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Send for Consumer<T, R> {}
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Sync for Consumer<T, R> {}

// ============================================================================
// ProducerByKey - Key-bound producer for multi-instance records
// ============================================================================

/// Type-safe producer bound to a specific record key
///
/// Unlike `Producer<T, R>` which uses type-based lookup, `ProducerByKey<T, R>`
/// is bound to a specific record key. This is necessary when multiple records
/// of the same type exist (e.g., "sensors.indoor" and "sensors.outdoor" both
/// storing `Temperature`).
///
/// # Type Parameters
/// * `T` - The record type this producer can emit
/// * `R` - The runtime adapter type
///
/// # Example
///
/// ```rust,ignore
/// // Get producers for different temperature sensors
/// let indoor = db.producer_by_key::<Temperature>("sensors.indoor");
/// let outdoor = db.producer_by_key::<Temperature>("sensors.outdoor");
///
/// // Each produces to its own record
/// indoor.produce(Temperature { celsius: 22.0 }).await?;
/// outdoor.produce(Temperature { celsius: -5.0 }).await?;
/// ```
#[cfg(feature = "alloc")]
pub struct ProducerByKey<T, R: aimdb_executor::Spawn + 'static> {
    /// Reference to the database
    db: Arc<AimDb<R>>,
    /// The record key this producer is bound to
    key: String,
    /// Phantom data to bind the type parameter T
    _phantom: PhantomData<T>,
}

#[cfg(feature = "alloc")]
impl<T, R> ProducerByKey<T, R>
where
    T: Send + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Create a new key-bound producer (internal use only)
    pub(crate) fn new(db: Arc<AimDb<R>>, key: String) -> Self {
        Self {
            db,
            key,
            _phantom: PhantomData,
        }
    }

    /// Produce a value to the bound record
    ///
    /// This writes the value to the specific record identified by the key
    /// provided during construction.
    pub async fn produce(&self, value: T) -> DbResult<()> {
        self.db.produce_by_key::<T>(&self.key, value).await
    }

    /// Returns the key this producer is bound to
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[cfg(feature = "alloc")]
impl<T, R> Clone for ProducerByKey<T, R>
where
    R: aimdb_executor::Spawn + 'static,
{
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            key: self.key.clone(),
            _phantom: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Send for ProducerByKey<T, R> {}
#[cfg(feature = "alloc")]
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Sync for ProducerByKey<T, R> {}

// Implement ProducerTrait for type-erased routing (key-based)
#[cfg(feature = "alloc")]
impl<T, R> crate::connector::ProducerTrait for ProducerByKey<T, R>
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

            // Produce the value to the key-bound record
            self.produce(*value)
                .await
                .map_err(|e| format!("Failed to produce value: {}", e))
        })
    }
}

// ============================================================================
// ConsumerByKey - Key-bound consumer for multi-instance records
// ============================================================================

/// Type-safe consumer bound to a specific record key
///
/// Unlike `Consumer<T, R>` which uses type-based lookup, `ConsumerByKey<T, R>`
/// is bound to a specific record key. This is necessary when multiple records
/// of the same type exist.
///
/// # Type Parameters
/// * `T` - The record type this consumer can subscribe to
/// * `R` - The runtime adapter type
///
/// # Example
///
/// ```rust,ignore
/// // Get consumers for different temperature sensors
/// let indoor = db.consumer_by_key::<Temperature>("sensors.indoor");
/// let outdoor = db.consumer_by_key::<Temperature>("sensors.outdoor");
///
/// // Each subscribes to its own record
/// let mut rx = indoor.subscribe()?;
/// while let Ok(temp) = rx.recv().await {
///     println!("Indoor: {:.1}°C", temp.celsius);
/// }
/// ```
#[cfg(feature = "alloc")]
pub struct ConsumerByKey<T, R: aimdb_executor::Spawn + 'static> {
    /// Reference to the database
    db: Arc<AimDb<R>>,
    /// The record key this consumer is bound to
    key: String,
    /// Phantom data to bind the type parameter T
    _phantom: PhantomData<T>,
}

#[cfg(feature = "alloc")]
impl<T, R> ConsumerByKey<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Create a new key-bound consumer (internal use only)
    pub(crate) fn new(db: Arc<AimDb<R>>, key: String) -> Self {
        Self {
            db,
            key,
            _phantom: PhantomData,
        }
    }

    /// Subscribe to updates for the bound record
    ///
    /// Returns a reader that yields values when they are produced
    /// to the specific record identified by the key.
    pub fn subscribe(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>> {
        self.db.subscribe_by_key::<T>(&self.key)
    }

    /// Returns the key this consumer is bound to
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[cfg(feature = "alloc")]
impl<T, R> Clone for ConsumerByKey<T, R>
where
    R: aimdb_executor::Spawn + 'static,
{
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            key: self.key.clone(),
            _phantom: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Send for ConsumerByKey<T, R> {}
#[cfg(feature = "alloc")]
unsafe impl<T: Send, R: aimdb_executor::Spawn + 'static> Sync for ConsumerByKey<T, R> {}

// ============================================================================
// Type-erased Consumer Trait Implementation
// ============================================================================

/// Adapter that wraps a typed BufferReader<T> and type-erases it
///
/// This allows the reader to be used through the AnyReader trait without
/// knowing the concrete type T at compile time.
struct TypedAnyReader<T: Clone + Send + 'static> {
    inner: Box<dyn crate::buffer::BufferReader<T> + Send>,
}

impl<T: Clone + Send + 'static> crate::connector::AnyReader for TypedAnyReader<T> {
    fn recv_any<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = DbResult<Box<dyn core::any::Any + Send>>> + Send + 'a>> {
        Box::pin(async move {
            let value = self.inner.recv().await?;
            Ok(Box::new(value) as Box<dyn core::any::Any + Send>)
        })
    }
}

/// Implement ConsumerTrait for type-erased routing
///
/// This allows connectors to subscribe to records without knowing the concrete
/// type T at compile time. The factory pattern captures T during link_to()
/// configuration, and this implementation provides the runtime subscription logic.
impl<T, R> crate::connector::ConsumerTrait for Consumer<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    fn subscribe_any<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = DbResult<Box<dyn crate::connector::AnyReader>>> + Send + 'a>>
    {
        Box::pin(async move {
            let reader = self.subscribe()?;
            Ok(Box::new(TypedAnyReader::<T> { inner: reader })
                as Box<dyn crate::connector::AnyReader>)
        })
    }
}

/// Implement ConsumerTrait for key-based type-erased routing
///
/// This allows connectors to subscribe to specific records by key without knowing
/// the concrete type T at compile time. Enables multiple records of the same type.
#[cfg(feature = "alloc")]
impl<T, R> crate::connector::ConsumerTrait for ConsumerByKey<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    fn subscribe_any<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = DbResult<Box<dyn crate::connector::AnyReader>>> + Send + 'a>>
    {
        Box::pin(async move {
            let reader = self.subscribe()?;
            Ok(Box::new(TypedAnyReader::<T> { inner: reader })
                as Box<dyn crate::connector::AnyReader>)
        })
    }
}

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
    /// The record key for this record (enables key-based routing)
    #[cfg(feature = "alloc")]
    pub(crate) record_key: String,
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

        // Store consumer factory that captures type T and record key
        // This allows the connector to subscribe to values without knowing T at compile time
        // Using key-based consumer enables multiple records of the same type
        #[cfg(feature = "alloc")]
        {
            let record_key = self.registrar.record_key.clone();
            link.consumer_factory = Some(Arc::new(
                move |db_any: Arc<dyn core::any::Any + Send + Sync>| {
                    // Downcast Arc<dyn Any> to AimDb<R>, then wrap in Arc
                    let db_ref = db_any
                        .downcast_ref::<AimDb<R>>()
                        .expect("Invalid db type in consumer factory");
                    let db = Arc::new(db_ref.clone());

                    // Create ConsumerByKey<T, R> with captured type T and record key
                    // This enables proper routing when multiple records share the same type
                    Box::new(ConsumerByKey::<T, R>::new(db, record_key.clone()))
                        as Box<dyn crate::connector::ConsumerTrait>
                },
            ));
        }

        // Store the connector link - consumers will be created later in build()
        // after connectors are actually built
        self.registrar.rec.add_outbound_connector(link);
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

        // Add producer factory callback that captures type T and record key (alloc feature)
        // Using key-based producer enables multiple records of the same type
        #[cfg(feature = "alloc")]
        {
            let record_key = self.registrar.record_key.clone();
            link = link.with_producer_factory(move |db_any| {
                // Downcast Arc<dyn Any> to Arc<AimDb<R>>
                let db = db_any
                    .downcast::<crate::builder::AimDb<R>>()
                    .expect("Failed to downcast to AimDb");
                // Create ProducerByKey<T, R> with captured type T and record key
                // This enables proper routing when multiple records share the same type
                Box::new(ProducerByKey::<T, R>::new(db, record_key.clone()))
                    as Box<dyn crate::connector::ProducerTrait>
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
