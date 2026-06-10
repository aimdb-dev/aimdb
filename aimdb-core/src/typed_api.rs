//! Type-safe Producer-Consumer API
//!
//! Provides the complete typed API for producer-consumer patterns including:
//! - `Producer<T>` - Type-safe value production
//! - `Consumer<T>` - Type-safe value consumption
//! - `RecordRegistrar` - Fluent record configuration API
//! - `RecordT` trait - Self-registering records
//!
//! # Producer Example
//!
//! ```rust,ignore
//! #[service]
//! async fn temperature_producer<R: Runtime>(
//!     ctx: RuntimeContext<R>,
//!     producer: Producer<Temperature>,
//! ) {
//!     loop {
//!         let temp = read_sensor().await;
//!         producer.produce(temp);
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
//!     consumer: Consumer<Temperature>,
//! ) {
//!     let mut rx = consumer.subscribe();
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
//!        .with_serializer_raw(|t| serde_json::to_vec(t))
//!        .finish();
//! });
//! ```

use core::fmt::Debug;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::buffer::{DynBuffer, WriteHandle};
use crate::typed_record::TypedRecord;
use crate::{AimDb, DbResult};

// ============================================================================
// Producer - Type-safe value production
// ============================================================================

/// Type-safe producer for a specific record type
///
/// `Producer<T>` provides scoped access to produce values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this producer can emit
///
/// Pre-binds the record's buffer, latest-snapshot slot, and metadata tracker
/// (via an internal `Arc<dyn WriteHandle<T>>`), so `produce()` is a single
/// virtual call rather than a `HashMap` lookup + downcast on each invocation
/// (design 029).
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it produces
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
pub struct Producer<T> {
    /// Pre-resolved write handle to the record's buffer/snapshot/metadata.
    write: Arc<dyn WriteHandle<T>>,
    /// Stage profiling state (set by the spawn machinery for `.source()` stages).
    #[cfg(feature = "profiling")]
    profiling: Option<Arc<crate::profiling::ProducerProfilingState>>,
    // `fn() -> T` carries T without forcing Producer's Send/Sync to depend on T.
    // T is only a type-system marker here — it is never stored or referenced.
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Producer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Create a new producer bound to a pre-resolved write handle.
    pub(crate) fn new(write: Arc<dyn WriteHandle<T>>) -> Self {
        Self {
            write,
            #[cfg(feature = "profiling")]
            profiling: None,
            _phantom: PhantomData,
        }
    }

    /// Attaches stage profiling state. Internal — called by the spawn machinery.
    #[cfg(feature = "profiling")]
    pub(crate) fn set_profiling(
        &mut self,
        metrics: Arc<crate::profiling::StageMetrics>,
        clock: crate::profiling::Clock,
    ) {
        self.profiling = Some(Arc::new(crate::profiling::ProducerProfilingState::new(
            metrics, clock,
        )));
    }

    /// Produce a value of type T
    ///
    /// Push to the record's buffer; consumer tasks and outbound link connectors
    /// observe it from there. Synchronous and infallible — the underlying
    /// `WriteHandle::push` cannot fail.
    ///
    pub fn produce(&self, value: T) {
        #[cfg(feature = "profiling")]
        if let Some(state) = &self.profiling {
            state.record_produce();
        }
        self.write.push(value);
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        Self {
            write: self.write.clone(),
            #[cfg(feature = "profiling")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
    }
}

// Implement ProducerTrait for type-erased routing
impl<T> crate::connector::ProducerTrait for Producer<T>
where
    T: Send + 'static + Debug + Clone,
{
    fn produce_any<'a>(
        &'a self,
        value: Box<dyn core::any::Any + Send>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // The only fallibility left is the type-erasure downcast; the
            // produce itself is synchronous + infallible after M14.
            let value = value.downcast::<T>().map_err(|_| {
                format!(
                    "Failed to downcast value to type {}",
                    core::any::type_name::<T>()
                )
            })?;
            self.produce(*value);
            Ok(())
        })
    }
}

// ============================================================================
// Consumer - Type-safe value consumption
// ============================================================================

/// Type-safe consumer for a specific record type
///
/// `Consumer<T>` provides scoped access to subscribe to values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this consumer can subscribe to
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it consumes
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
pub struct Consumer<T> {
    /// Pre-resolved buffer handle to the record's buffer.
    buffer: Arc<dyn DynBuffer<T>>,
    /// Stage profiling state (set by the spawn machinery for `.tap()` / `.link_to()`).
    #[cfg(feature = "profiling")]
    profiling: Option<(Arc<crate::profiling::StageMetrics>, crate::profiling::Clock)>,
    // See Producer<T>: `fn() -> T` keeps Send/Sync independent of T.
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Consumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Create a new consumer bound to a pre-resolved buffer handle.
    pub(crate) fn new(buffer: Arc<dyn DynBuffer<T>>) -> Self {
        Self {
            buffer,
            #[cfg(feature = "profiling")]
            profiling: None,
            _phantom: PhantomData,
        }
    }

    /// Attaches stage profiling state. Internal — called by the spawn machinery.
    #[cfg(feature = "profiling")]
    pub(crate) fn set_profiling(
        &mut self,
        metrics: Arc<crate::profiling::StageMetrics>,
        clock: crate::profiling::Clock,
    ) {
        self.profiling = Some((metrics, clock));
    }

    /// Subscribe to updates for this record type.
    ///
    /// Returns a reader that yields values as they are produced.
    /// Infallible — the buffer is pre-resolved at `Consumer` construction.
    pub fn subscribe(&self) -> Box<dyn crate::buffer::BufferReader<T> + Send> {
        let reader = self.buffer.subscribe_boxed();
        #[cfg(feature = "profiling")]
        if let Some((metrics, clock)) = &self.profiling {
            return Box::new(crate::profiling::ProfilingBufferReader::new(
                reader,
                metrics.clone(),
                clock.clone(),
            ));
        }
        reader
    }
}

impl<T> Clone for Consumer<T> {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            #[cfg(feature = "profiling")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
    }
}

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
impl<T> crate::connector::ConsumerTrait for Consumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn subscribe_any<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Box<dyn crate::connector::AnyReader>> + Send + 'a>> {
        Box::pin(async move {
            let reader = self.subscribe();
            Box::new(TypedAnyReader::<T> { inner: reader }) as Box<dyn crate::connector::AnyReader>
        })
    }
}

// ============================================================================
// RecordRegistrar - Fluent registration API
// ============================================================================

/// Type alias for typed serializer callbacks
type TypedSerializerFn<T> =
    Arc<dyn Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static>;

/// Kind of execution stage, used to address per-stage profiling metrics and to
/// remember which stage `RecordRegistrar::with_name` should rename.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageKind {
    /// A `.source()` producer callback.
    Source,
    /// A `.tap()` observer callback.
    Tap,
    /// An outbound `.link_to()` connector.
    Link,
    /// A `.transform()` callback (reserved; not yet instrumented).
    Transform,
}

/// Registrar for configuring a typed record
///
/// Provides a fluent API for registering producer and consumer functions.
pub struct RecordRegistrar<'a, T: Send + Sync + 'static + Debug + Clone> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T>,
    /// Connector builders indexed by scheme
    pub(crate) connector_builders: &'a [Box<dyn crate::connector::ConnectorBuilder>],
    /// The record key for this record
    pub(crate) record_key: String,
    /// Extension storage from the builder — allows external crates (e.g.
    /// `aimdb-persistence`) to retrieve typed state inside `.persist()`.
    pub(crate) extensions: &'a crate::extensions::Extensions,
    /// The most recently registered stage, so `.with_name()` knows what to name.
    /// Tracked even when the `profiling` feature is off (then it's just unused).
    #[cfg_attr(not(feature = "profiling"), allow(dead_code))]
    pub(crate) last_stage: Option<(StageKind, usize)>,
}

impl<'a, T> RecordRegistrar<'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Returns a reference to the builder's extension storage.
    ///
    /// External crates call this inside their `.persist()` (or similar) extension
    /// methods to retrieve typed state that was stored via
    /// `builder.extensions_mut().insert(...)` before `configure()` was called.
    pub fn extensions(&self) -> &crate::extensions::Extensions {
        self.extensions
    }

    /// Assigns a human-readable name to the stage registered immediately before
    /// this call (the most recent `.source()`, `.tap()`, or `.link_to()`).
    ///
    /// The name shows up in stage profiling output. This method is always
    /// available; when the `profiling` feature is disabled it is a no-op.
    ///
    /// ```rust,ignore
    /// reg.source(|ctx, producer| async move { /* ... */ })
    ///    .with_name("sensor_reader");
    /// ```
    pub fn with_name(&mut self, name: &str) -> &mut Self {
        #[cfg(feature = "profiling")]
        if let Some((kind, idx)) = self.last_stage {
            self.rec.profiling_mut().set_stage_name(kind, idx, name);
        }
        #[cfg(not(feature = "profiling"))]
        let _ = name;
        self
    }

    /// Registers a producer service for this record type.
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext)
    /// (time + logging capabilities) and a pre-resolved [`Producer<T>`]; it is
    /// collected at `build()` time and driven by the `AimDbRunner`.
    ///
    /// ```rust,ignore
    /// reg.source(|ctx, producer| async move {
    ///     loop {
    ///         producer.produce(read_sensor().await);
    ///         ctx.time().sleep_secs(1).await;
    ///     }
    /// });
    /// ```
    pub fn source<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(crate::RuntimeContext, crate::Producer<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.set_producer(f);
        #[cfg(feature = "profiling")]
        {
            let (idx, _) = self.rec.profiling_mut().push_source();
            self.last_stage = Some((StageKind::Source, idx));
        }
        #[cfg(not(feature = "profiling"))]
        {
            self.last_stage = Some((StageKind::Source, 0));
        }
        self
    }

    /// Register a side-effect observer that taps into the data stream.
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) and a
    /// pre-resolved [`Consumer<T>`]; it is collected at `build()` time and
    /// driven by the `AimDbRunner`. Multiple taps per record are allowed.
    ///
    /// ```rust,ignore
    /// reg.tap(|ctx, consumer| async move {
    ///     let mut rx = consumer.subscribe();
    ///     while let Ok(value) = rx.recv().await {
    ///         ctx.log().info("observed value");
    ///     }
    /// });
    /// ```
    pub fn tap<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(crate::RuntimeContext, crate::Consumer<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        T: Sync,
    {
        self.rec.add_consumer(f);
        #[cfg(feature = "profiling")]
        {
            let (idx, _) = self.rec.profiling_mut().push_tap();
            self.last_stage = Some((StageKind::Tap, idx));
        }
        #[cfg(not(feature = "profiling"))]
        {
            self.last_stage = Some((StageKind::Tap, 0));
        }
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
    pub fn buffer_raw(&mut self, buffer: Box<dyn crate::buffer::DynBuffer<T>>) -> &mut Self {
        self.rec.set_buffer(buffer);
        self
    }

    /// Configures a buffer with metadata tracking
    pub fn buffer_with_cfg(
        &mut self,
        buffer: Box<dyn crate::buffer::DynBuffer<T>>,
        cfg: crate::buffer::BufferCfg,
    ) -> &mut Self {
        self.rec.set_buffer(buffer);
        self.rec.set_buffer_cfg(cfg);
        self
    }

    /// Sets the buffer configuration for metadata tracking
    pub fn buffer_cfg(&mut self, cfg: crate::buffer::BufferCfg) -> &mut Self {
        self.rec.set_buffer_cfg(cfg);
        self
    }

    /// Installs the JSON codec for this record (feature `json-serialize`)
    ///
    /// Enables `record.latest()?.as_json()`, and on `std` the AimX `record.get`
    /// / `set` / `subscribe` protocol. Requires `T: RemoteSerialize`
    /// (blanket-impl'd for every `Serialize + DeserializeOwned` type). Works on
    /// no_std + alloc.
    ///
    /// # Example
    /// ```rust,ignore
    /// builder.configure::<Temperature>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .with_remote_access();  // Enable remote queries
    /// });
    /// ```
    #[cfg(feature = "json-serialize")]
    pub fn with_remote_access(&mut self) -> &mut Self
    where
        T: crate::codec::RemoteSerialize + 'static,
    {
        self.rec.with_remote_access();
        self
    }

    /// Register a single-input reactive transform.
    ///
    /// Conflicts with `.source()` and other `.transform()`s are recorded and
    /// reported from `build()`.
    ///
    /// # Type Parameters
    /// * `I` - The input record type to subscribe to
    ///
    /// # Arguments
    /// * `input_key` - The record key to subscribe to as input
    /// * `build_fn` - Closure that configures the transform pipeline via `TransformBuilder`
    pub fn transform<I, F>(&mut self, input_key: impl crate::RecordKey, build_fn: F) -> &mut Self
    where
        I: Send + Sync + Clone + Debug + 'static,
        F: FnOnce(
            crate::transform::TransformBuilder<I, T>,
        ) -> crate::transform::TransformPipeline<I, T>,
    {
        let input_key_str = input_key.as_str().to_string();
        let builder = crate::transform::TransformBuilder::<I, T>::new(input_key_str);
        let pipeline = build_fn(builder);
        let descriptor = pipeline.into_descriptor();
        self.rec.set_transform(descriptor);
        // Transform stages are not yet instrumented; track the kind so `.with_name()`
        // remains coherent (it currently has nowhere to store the name).
        self.last_stage = Some((StageKind::Transform, 0));
        self
    }

    /// Multi-input reactive transform (join).
    ///
    /// Derives this record from multiple input records. Available on every
    /// runtime. Conflicts with `.source()` and other `.transform()`s are
    /// recorded and reported from `build()`.
    pub fn transform_join<F>(&mut self, build_fn: F) -> &mut Self
    where
        F: FnOnce(crate::transform::JoinBuilder<T>) -> crate::transform::JoinPipeline<T>,
    {
        let builder = crate::transform::JoinBuilder::<T>::new();
        let pipeline = build_fn(builder);
        let descriptor = pipeline.into_descriptor();
        self.rec.set_transform(descriptor);
        self.last_stage = Some((StageKind::Transform, 0));
        self
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
    ///            .with_serializer_raw(|t| serde_json::to_vec(t).unwrap())
    ///            .finish()
    /// });
    /// ```
    pub fn link_to(&mut self, url: &str) -> OutboundConnectorBuilder<'_, 'a, T> {
        OutboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            serializer: None,
            context_serializer: None,
            topic_provider: None,
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
    ///            .with_deserializer(|_ctx, bytes: &[u8]| parse_light_state(bytes))
    ///            .finish()
    /// });
    /// ```
    pub fn link_from(&mut self, url: &str) -> InboundConnectorBuilder<'_, 'a, T> {
        InboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            deserializer: None,
            context_deserializer: None,
            topic_resolver: None,
        }
    }
}

// ============================================================================
// OutboundConnectorBuilder - Fluent outbound connector configuration
// ============================================================================

/// Builder for configuring outbound connector links (AimDB → External)
///
/// `'r` is the borrow of the registrar taken by `link_to()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub struct OutboundConnectorBuilder<'r, 'a, T: Send + Sync + 'static + Debug + Clone> {
    registrar: &'r mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    serializer: Option<TypedSerializerFn<T>>,
    context_serializer: Option<crate::connector::ContextSerializerFn>,
    topic_provider: Option<crate::connector::TopicProviderFn>,
}

impl<'r, 'a, T> OutboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Adds a configuration option to the connector
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Sets a raw serialization callback (value only, no context)
    ///
    /// Prefer `.with_serializer(|ctx, value| ...)` for access to
    /// `RuntimeContext` (timestamps, logging). Use this raw variant
    /// only when context is unnecessary.
    pub fn with_serializer_raw<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Result<Vec<u8>, crate::connector::SerializeError> + Send + Sync + 'static,
    {
        self.serializer = Some(Arc::new(f));
        self.context_serializer = None; // mutually exclusive
        self
    }

    /// Sets a context-aware serialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the typed value being
    /// serialized.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .link_to("mqtt://broker/sensors/temp")
    ///     .with_serializer(|ctx, value: &Temperature| {
    ///         ctx.log().debug("Serializing temperature for MQTT");
    ///         value.to_bytes()
    ///             .map_err(|_| SerializeError::InvalidData)
    ///     })
    /// ```
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
            + Send
            + Sync
            + 'static,
    {
        let f = Arc::new(f);
        self.context_serializer = Some(Arc::new(move |ctx, value_any| {
            if let Some(value) = value_any.downcast_ref::<T>() {
                (f)(ctx, value)
            } else {
                Err(crate::connector::SerializeError::TypeMismatch)
            }
        }));
        self.serializer = None; // mutually exclusive
        self
    }

    /// Sets the operation timeout in milliseconds (the connector interprets
    /// it; passed as the `timeout_ms` option — see
    /// `ConnectorConfig::from_query`)
    ///
    /// Protocol-specific knobs (e.g. MQTT QoS/retain) are provided by the
    /// connector crates as extension traits over this builder, or generically
    /// via [`with_config`](Self::with_config).
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Sets a dynamic topic provider
    ///
    /// The provider receives the value being published and returns
    /// the topic/destination to publish to. Return `None` to use the default
    /// static topic from the URL.
    ///
    /// # Type Safety
    ///
    /// The provider is type-checked at compile time against `T`.
    /// Type erasure occurs internally for storage.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::connector::TopicProvider;
    ///
    /// struct SensorTopicProvider;
    ///
    /// impl TopicProvider<Temperature> for SensorTopicProvider {
    ///     fn topic(&self, value: &Temperature) -> Option<String> {
    ///         Some(format!("sensors/temp/{}", value.sensor_id))
    ///     }
    /// }
    ///
    /// reg.link_to("mqtt://sensors/default")
    ///    .with_topic_provider(SensorTopicProvider)
    ///    .with_serializer(...)
    ///    .finish();
    /// ```
    pub fn with_topic_provider<P>(mut self, provider: P) -> Self
    where
        P: crate::connector::TopicProvider<T> + 'static,
    {
        // Type-erase the provider via TopicProviderWrapper
        self.topic_provider = Some(Arc::new(crate::connector::TopicProviderWrapper::new(
            provider,
        )));
        self
    }

    /// Finalizes the connector registration
    ///
    /// Configuration mistakes — an invalid URL, a missing serializer, or an
    /// unregistered scheme — are recorded instead of panicking: the link is
    /// **not** registered and `build()` reports every finding via
    /// `DbError::InvalidConfiguration`. The registrar is returned either way
    /// so chained configuration keeps compiling; the failed `build()` is the
    /// single error surface.
    ///
    /// The buffer requirement is validated by `build()` (calling `.buffer()`
    /// after `.link_to()` is fine).
    pub fn finish(self) -> &'r mut RecordRegistrar<'a, T> {
        use crate::connector::{ConnectorLink, ConnectorUrl};
        use crate::error::ConfigError;

        let record_key = self.registrar.record_key.clone();

        let Ok(url) = ConnectorUrl::parse(&self.url) else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Invalid connector URL",
            ));
            return self.registrar;
        };

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        let mut link = ConnectorLink::new(url.clone());
        link.config = self.config.clone();

        // Resolve serializer variant (mutually exclusive)
        let ser_kind = if let Some(ctx_ser) = self.context_serializer {
            crate::connector::SerializerKind::Context(ctx_ser)
        } else if let Some(raw_ser) = self.serializer.clone() {
            // Type-erase the raw serializer
            let erased: crate::connector::SerializerFn =
                Arc::new(move |any: &dyn core::any::Any| {
                    if let Some(value) = any.downcast_ref::<T>() {
                        (raw_ser)(value)
                    } else {
                        Err(crate::connector::SerializeError::TypeMismatch)
                    }
                });
            crate::connector::SerializerKind::Raw(erased)
        } else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Outbound connector requires a serializer. \
                 Call .with_serializer() or .with_serializer_raw()",
            ));
            return self.registrar;
        };

        link.serializer = Some(ser_kind);

        // Wire through the topic provider
        link.topic_provider = self.topic_provider;

        // Validation: Check that connector builder is registered
        let has_connector = self
            .registrar
            .connector_builders
            .iter()
            .any(|b| b.scheme() == scheme);

        if !has_connector {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(url_string),
                alloc::format!(
                    "No connector registered for scheme '{scheme}'. Register via .with_connector()"
                ),
            ));
            return self.registrar;
        }

        // Register the link as a profiling stage (so `.with_name()` can name it
        // and the consumer it creates can be timed).
        #[cfg(feature = "profiling")]
        let link_metrics = {
            let (idx, metrics) = self.registrar.rec.profiling_mut().push_link();
            self.registrar.last_stage = Some((StageKind::Link, idx));
            metrics
        };
        #[cfg(not(feature = "profiling"))]
        {
            self.registrar.last_stage = Some((StageKind::Link, 0));
        }

        // Store consumer factory that captures type T and record key
        // This allows the connector to subscribe to values without knowing T at compile time.
        //
        // Resolves the record at link-startup time (not per-message) and constructs a
        // `Consumer<T>` bound to a pre-resolved buffer handle — same pattern as the
        // build-time path in `TypedRecord::collect_consumer_futures` (design 029).
        //
        // The factory runs during build() after every record is registered and
        // validated (including the linked-records-need-a-buffer check), so
        // failures here are aimdb bugs, not user mistakes.
        {
            let record_key = self.registrar.record_key.clone();
            link.consumer_factory = Some(Arc::new(move |db: &AimDb| {
                let typed_rec = db
                    .inner()
                    .get_typed_record_by_key::<T>(&record_key)
                    .unwrap_or_else(|e| {
                        panic!(
                            "consumer factory: record '{record_key}' lookup failed ({e:?}) — \
                             this is a bug in aimdb-core"
                        )
                    });
                let buffer = typed_rec.buffer_handle().unwrap_or_else(|| {
                    panic!(
                        "consumer factory: record '{record_key}' has no buffer despite \
                         build()-time validation — this is a bug in aimdb-core"
                    )
                });

                #[allow(unused_mut)]
                let mut consumer = Consumer::<T>::new(buffer);
                #[cfg(feature = "profiling")]
                consumer.set_profiling(link_metrics.clone(), db.profiling_clock().clone());
                Box::new(consumer) as Box<dyn crate::connector::ConsumerTrait>
            }));
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
///
/// `'r` is the borrow of the registrar taken by `link_from()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub struct InboundConnectorBuilder<'r, 'a, T: Send + Sync + 'static + Debug + Clone> {
    registrar: &'r mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    deserializer: Option<TypedDeserializerFn<T>>,
    context_deserializer: Option<crate::connector::ContextDeserializerFn>,
    topic_resolver: Option<crate::connector::TopicResolverFn>,
}

impl<'r, 'a, T> InboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Adds a configuration option to the connector
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Sets a raw deserialization callback (bytes only, no context)
    ///
    /// Prefer `.with_deserializer(|ctx, data| ...)` for access to
    /// `RuntimeContext` (timestamps, logging). Use this raw variant
    /// only when context is unnecessary.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .link_from("mqtt://broker/sensors/temp")
    ///     .with_deserializer_raw(|bytes| {
    ///         serde_json::from_slice::<Temperature>(bytes)
    ///             .map_err(|e| e.to_string())
    ///     })
    /// ```
    pub fn with_deserializer_raw<F>(mut self, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        self.deserializer = Some(Arc::new(f));
        self.context_deserializer = None; // mutually exclusive
        self
    }

    /// Sets a context-aware deserialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the raw bytes from
    /// the external system.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .link_from("knx://gateway/9/1/0")
    ///     .with_deserializer(|ctx, data: &[u8]| {
    ///         let mut temp = from_knx(data, "9/1/0")?;
    ///         temp.timestamp = ctx.time().now();
    ///         Ok(temp)
    ///     })
    /// ```
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        self.context_deserializer = Some(Arc::new(move |ctx, bytes| {
            (f)(ctx, bytes).map(|val| Box::new(val) as Box<dyn core::any::Any + Send>)
        }));
        self.deserializer = None; // mutually exclusive
        self
    }

    /// Sets the operation timeout in milliseconds (the connector interprets
    /// it; passed as the `timeout_ms` option — see
    /// `ConnectorConfig::from_query`)
    ///
    /// Protocol-specific knobs (e.g. MQTT subscribe QoS) are provided by the
    /// connector crates as extension traits over this builder, or generically
    /// via [`with_config`](Self::with_config).
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Sets a dynamic topic resolver for late-binding scenarios
    ///
    /// The resolver is called once at connector startup to determine
    /// the subscription topic. Return `None` to use the default
    /// static topic from the URL.
    ///
    /// # Use Cases
    ///
    /// - Topics determined from smart contracts at runtime
    /// - Service discovery integration
    /// - Environment-specific topic configuration
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.link_from("mqtt://mesh/default/data")  // Fallback topic
    ///    .with_topic_resolver(|| {
    ///        // Read from smart contract, config service, etc.
    ///        let node_id = smart_contract.get_producer_node_id()?;
    ///        Some(format!("mesh/{}/data", node_id))
    ///    })
    ///    .with_deserializer(|_ctx, bytes: &[u8]| parse_sensor_data(bytes))
    ///    .finish();
    /// ```
    pub fn with_topic_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.topic_resolver = Some(Arc::new(resolver));
        self
    }

    /// Finalizes the inbound connector registration
    ///
    /// Configuration mistakes — an invalid URL, a missing deserializer, an
    /// unregistered scheme, or a conflict with `.source()`/`.transform()`
    /// (local producer + inbound connector would race as last-writer-wins) —
    /// are recorded instead of panicking: the link is **not** registered and
    /// `build()` reports every finding via `DbError::InvalidConfiguration`.
    /// The registrar is returned either way so chained configuration keeps
    /// compiling; the failed `build()` is the single error surface.
    ///
    /// The buffer requirement is validated by `build()` (calling `.buffer()`
    /// after `.link_from()` is fine).
    pub fn finish(self) -> &'r mut RecordRegistrar<'a, T> {
        use crate::connector::{ConnectorUrl, DeserializerKind, InboundConnectorLink};
        use crate::error::ConfigError;

        let record_key = self.registrar.record_key.clone();

        let Ok(url) = ConnectorUrl::parse(&self.url) else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Invalid connector URL",
            ));
            return self.registrar;
        };

        let scheme = url.scheme().to_string();

        // NOTE: the buffer requirement is validated by `build()`, not here —
        // `.buffer()` may legitimately be called after `.link_from()`.

        // Mutual exclusion with local producers — both write to the same
        // buffer and would race as last-writer-wins. The check here carries
        // the URL; `add_inbound_connector` enforces the same invariant from
        // the other direction.
        if self.registrar.rec.has_transform() {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Record already has a .transform(); cannot also have a .link_from().",
            ));
            return self.registrar;
        }
        if self.registrar.rec.has_producer() {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Record already has a .source(); cannot also have a .link_from().",
            ));
            return self.registrar;
        }

        // Resolve deserializer variant (mutually exclusive)
        let deser_kind = if let Some(ctx_deser) = self.context_deserializer {
            DeserializerKind::Context(ctx_deser)
        } else if let Some(raw_deser) = self.deserializer {
            // Type-erase the raw deserializer
            let erased: crate::connector::DeserializerFn = Arc::new(move |bytes: &[u8]| {
                raw_deser(bytes).map(|val| Box::new(val) as Box<dyn core::any::Any + Send>)
            });
            DeserializerKind::Raw(erased)
        } else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Inbound connector requires a deserializer. \
                 Call .with_deserializer() or .with_deserializer_raw()",
            ));
            return self.registrar;
        };

        // Validation: Connector builder must be registered
        let has_connector = self
            .registrar
            .connector_builders
            .iter()
            .any(|b| b.scheme() == scheme);

        if !has_connector {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                alloc::format!(
                    "No connector registered for scheme '{scheme}'. Register via .with_connector()"
                ),
            ));
            return self.registrar;
        }

        // Create inbound connector link
        let mut link = InboundConnectorLink::new(url, deser_kind);
        link.config = self.config;

        // Wire through the topic resolver
        link.topic_resolver = self.topic_resolver;

        // Add producer factory callback that captures type T and record key.
        // The factory runs during build() after every record is registered and
        // validated, so failures here are aimdb bugs, not user mistakes.
        {
            let record_key = self.registrar.record_key.clone();
            link = link.with_producer_factory(move |db: &AimDb| {
                let typed_rec = db
                    .inner()
                    .get_typed_record_by_key::<T>(&record_key)
                    .unwrap_or_else(|e| {
                        panic!(
                            "producer factory: record '{record_key}' lookup failed ({e:?}) — \
                             this is a bug in aimdb-core"
                        )
                    });
                Box::new(Producer::<T>::new(typed_rec.writer_handle()))
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
pub trait RecordT: Send + Sync + 'static + Debug + Clone {
    /// Configuration type for this record
    type Config;

    /// Registers producer and consumer functions
    fn register(reg: &mut RecordRegistrar<'_, Self>, cfg: &Self::Config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "std"))]
    use alloc::vec;

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

    // ====================================================================
    // Test infrastructure for InboundConnectorBuilder deserializer tests
    // ====================================================================

    /// Minimal mock runtime for context tests
    struct MockRuntime;

    impl aimdb_executor::RuntimeAdapter for MockRuntime {
        fn runtime_name() -> &'static str {
            "mock"
        }
    }

    impl aimdb_executor::TimeOps for MockRuntime {
        type Instant = u64;
        type Duration = u64;
        fn now(&self) -> u64 {
            0
        }
        fn duration_since(&self, _later: u64, _earlier: u64) -> Option<u64> {
            Some(0)
        }
        fn millis(&self, ms: u64) -> u64 {
            ms
        }
        fn secs(&self, secs: u64) -> u64 {
            secs * 1000
        }
        fn micros(&self, micros: u64) -> u64 {
            micros
        }
        fn sleep(&self, _duration: u64) -> impl Future<Output = ()> + Send {
            core::future::ready(())
        }
        fn duration_as_nanos(&self, duration: u64) -> u64 {
            duration
        }
    }

    impl aimdb_executor::Logger for MockRuntime {
        fn info(&self, _message: &str) {}
        fn debug(&self, _message: &str) {}
        fn warn(&self, _message: &str) {}
        fn error(&self, _message: &str) {}
    }

    impl aimdb_executor::RuntimeOps for MockRuntime {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn now_nanos(&self) -> u64 {
            0
        }
        fn unix_time(&self) -> Option<(u64, u32)> {
            None
        }
        fn sleep(&self, _d: core::time::Duration) -> aimdb_executor::BoxFuture {
            Box::pin(core::future::ready(()))
        }
        fn log(&self, _level: aimdb_executor::LogLevel, _msg: &str) {}
    }

    /// Minimal mock buffer so `has_buffer()` returns true
    struct MockBuffer;

    impl crate::buffer::DynBuffer<TestRecord> for MockBuffer {
        fn push(&self, _value: TestRecord) {}
        fn subscribe_boxed(&self) -> Box<dyn crate::buffer::BufferReader<TestRecord> + Send> {
            unimplemented!("not needed for deserializer tests")
        }
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    /// Mock connector builder that reports a given scheme
    struct MockConnectorBuilder {
        scheme: String,
    }

    impl crate::connector::ConnectorBuilder for MockConnectorBuilder {
        fn build<'a>(
            &'a self,
            _db: &'a crate::AimDb,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>,
                    > + Send
                    + 'a,
            >,
        > {
            unimplemented!("not needed for deserializer tests")
        }
        fn scheme(&self) -> &str {
            &self.scheme
        }
    }

    /// Helper: build a RecordRegistrar wired to a TypedRecord with a buffer and a
    /// mock connector builder for the given scheme.
    fn make_registrar<'a>(
        rec: &'a mut crate::typed_record::TypedRecord<TestRecord>,
        builders: &'a [Box<dyn crate::connector::ConnectorBuilder>],
        extensions: &'a crate::extensions::Extensions,
    ) -> RecordRegistrar<'a, TestRecord> {
        RecordRegistrar {
            rec,
            connector_builders: builders,
            record_key: "test::Record".to_string(),
            extensions,
            last_stage: None,
        }
    }

    // ====================================================================
    // Deserializer-kind selection tests
    // ====================================================================

    #[test]
    fn inbound_finish_stores_raw_deserializer_kind() {
        use crate::connector::DeserializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_from("mqtt://broker/topic")
            .with_deserializer_raw(|bytes: &[u8]| {
                Ok(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();

        assert_eq!(rec.inbound_connectors().len(), 1);
        let link = &rec.inbound_connectors()[0];

        // Variant must be Raw
        assert!(
            matches!(link.deserializer, DeserializerKind::Raw(_)),
            "expected DeserializerKind::Raw, got Context"
        );

        // Verify the type-erased deserializer round-trips correctly
        if let DeserializerKind::Raw(ref f) = link.deserializer {
            let result = f(&[1, 2, 3]).expect("deserializer should succeed");
            let record = result
                .downcast::<TestRecord>()
                .expect("should downcast to TestRecord");
            assert_eq!(record.value, 3);
        }
    }

    #[test]
    fn inbound_finish_stores_context_deserializer_kind() {
        use crate::connector::DeserializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_from("mqtt://broker/topic")
            .with_deserializer(|_ctx: crate::RuntimeContext, bytes: &[u8]| {
                Ok(TestRecord {
                    value: bytes.len() as i32 * 10,
                })
            })
            .finish();

        assert_eq!(rec.inbound_connectors().len(), 1);

        assert!(
            matches!(
                rec.inbound_connectors()[0].deserializer,
                DeserializerKind::Context(_)
            ),
            "expected DeserializerKind::Context, got Raw"
        );
    }

    #[test]
    fn inbound_raw_overrides_previous_context_deserializer() {
        use crate::connector::DeserializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // Set context first, then override with raw — raw should win
        reg.link_from("mqtt://broker/topic")
            .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                Ok(TestRecord { value: 0 })
            })
            .with_deserializer_raw(|bytes: &[u8]| {
                Ok(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();

        assert!(matches!(
            rec.inbound_connectors()[0].deserializer,
            DeserializerKind::Raw(_)
        ));
    }

    #[test]
    fn inbound_context_overrides_previous_raw_deserializer() {
        use crate::connector::DeserializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // Set raw first, then override with context — context should win
        reg.link_from("mqtt://broker/topic")
            .with_deserializer_raw(|_bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                Ok(TestRecord { value: 99 })
            })
            .finish();

        assert!(matches!(
            rec.inbound_connectors()[0].deserializer,
            DeserializerKind::Context(_)
        ));
    }

    /// Drains the configuration errors a registrar/setter recorded on `rec`.
    fn drain_errors(
        rec: &mut crate::typed_record::TypedRecord<TestRecord>,
    ) -> Vec<crate::error::ConfigError> {
        use crate::typed_record::AnyRecord;
        rec.drain_config_errors()
    }

    #[test]
    fn inbound_finish_without_deserializer_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // No deserializer set — error recorded, link not registered
        reg.link_from("mqtt://broker/topic").finish();

        assert!(rec.inbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Inbound connector requires a deserializer"));
        assert_eq!(errors[0].record_key, "test::Record");
        assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
    }

    // ====================================================================
    // Serializer-kind selection tests
    // ====================================================================

    #[test]
    fn outbound_finish_stores_raw_serializer_kind() {
        use crate::connector::SerializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_to("mqtt://broker/topic")
            .with_serializer_raw(|record: &TestRecord| Ok(record.value.to_le_bytes().to_vec()))
            .finish();

        assert_eq!(rec.outbound_connectors().len(), 1);
        let link = &rec.outbound_connectors()[0];

        // Variant must be Raw
        let ser = link.serializer.as_ref().expect("serializer should be set");
        assert!(
            matches!(ser, SerializerKind::Raw(_)),
            "expected SerializerKind::Raw, got Context"
        );

        // Verify the type-erased serializer round-trips correctly
        if let SerializerKind::Raw(ref f) = ser {
            let val = TestRecord { value: 42 };
            let result = f(&val as &dyn core::any::Any).expect("serializer should succeed");
            assert_eq!(result, 42i32.to_le_bytes().to_vec());
        }
    }

    #[test]
    fn outbound_finish_stores_context_serializer_kind() {
        use crate::connector::SerializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_to("mqtt://broker/topic")
            .with_serializer(|_ctx: crate::RuntimeContext, record: &TestRecord| {
                Ok(record.value.to_le_bytes().to_vec())
            })
            .finish();

        assert_eq!(rec.outbound_connectors().len(), 1);
        let ser = rec.outbound_connectors()[0]
            .serializer
            .as_ref()
            .expect("serializer should be set");

        assert!(
            matches!(ser, SerializerKind::Context(_)),
            "expected SerializerKind::Context, got Raw"
        );
    }

    #[test]
    fn outbound_raw_overrides_previous_context_serializer() {
        use crate::connector::SerializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // Set context first, then override with raw — raw should win
        reg.link_to("mqtt://broker/topic")
            .with_serializer(|_ctx: crate::RuntimeContext, _record: &TestRecord| Ok(vec![0]))
            .with_serializer_raw(|record: &TestRecord| Ok(record.value.to_le_bytes().to_vec()))
            .finish();

        let ser = rec.outbound_connectors()[0]
            .serializer
            .as_ref()
            .expect("serializer should be set");
        assert!(matches!(ser, SerializerKind::Raw(_)));
    }

    #[test]
    fn outbound_context_overrides_previous_raw_serializer() {
        use crate::connector::SerializerKind;

        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // Set raw first, then override with context — context should win
        reg.link_to("mqtt://broker/topic")
            .with_serializer_raw(|_record: &TestRecord| Ok(vec![0]))
            .with_serializer(|_ctx: crate::RuntimeContext, _record: &TestRecord| Ok(vec![99]))
            .finish();

        let ser = rec.outbound_connectors()[0]
            .serializer
            .as_ref()
            .expect("serializer should be set");
        assert!(matches!(ser, SerializerKind::Context(_)));
    }

    #[test]
    fn outbound_finish_without_serializer_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // No serializer set — error recorded, link not registered
        reg.link_to("mqtt://broker/topic").finish();

        assert!(rec.outbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Outbound connector requires a serializer"));
        assert_eq!(errors[0].record_key, "test::Record");
        assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
    }

    #[test]
    fn finish_with_unregistered_scheme_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        // No connector builders registered at all
        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> = vec![];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);
        reg.link_to("mqtt://broker/topic")
            .with_serializer_raw(|r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
            .finish();

        assert!(rec.outbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("No connector registered for scheme 'mqtt'"));
        assert_eq!(errors[0].record_key, "test::Record");
    }

    // ====================================================================
    // Writer-exclusivity tests (.source / .transform / .link_from)
    // ====================================================================

    /// Helper: build a `TransformDescriptor` with a no-op spawn function.
    fn dummy_transform_descriptor() -> crate::transform::TransformDescriptor<TestRecord> {
        crate::transform::TransformDescriptor::<TestRecord> {
            input_keys: vec![],
            build_fn: Box::new(
                |_p, _db, _output_key| crate::transform::CollectedTransform {
                    task_future: Box::pin(async {}),
                    fanin_futures: vec![],
                },
            ),
        }
    }

    #[test]
    fn link_from_after_source_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));
        rec.set_producer(|_ctx, _p| async move {});

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);
        reg.link_from("mqtt://broker/topic")
            .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();

        assert!(rec.inbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Record already has a .source(); cannot also have a .link_from()."));
        assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
    }

    #[test]
    fn link_from_after_transform_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));
        rec.set_transform(dummy_transform_descriptor());

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);
        reg.link_from("mqtt://broker/topic")
            .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();

        assert!(rec.inbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Record already has a .transform(); cannot also have a .link_from()."));
        assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
    }

    #[test]
    fn source_after_link_from_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();
        {
            let mut reg = make_registrar(&mut rec, &builders, &extensions);
            reg.link_from("mqtt://broker/topic")
                .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
        }

        rec.set_producer(|_ctx, _p| async move {});

        assert!(!rec.has_producer(), "conflicting producer must be skipped");
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Record already has a .link_from(); cannot also have a .source()."));
    }

    #[test]
    fn transform_after_link_from_records_error() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();
        {
            let mut reg = make_registrar(&mut rec, &builders, &extensions);
            reg.link_from("mqtt://broker/topic")
                .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
        }

        rec.set_transform(dummy_transform_descriptor());

        assert!(
            !rec.has_transform(),
            "conflicting transform must be skipped"
        );
        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("Record already has a .link_from(); cannot also have a .transform()."));
    }

    #[test]
    fn multiple_link_from_allowed() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();
        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        // Chained via finish() → &mut RecordRegistrar …
        reg.link_from("mqtt://broker/topic-a")
            .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish()
            .link_from("mqtt://broker/topic-b")
            .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();

        // … and as separate statements: each call takes a fresh borrow, so
        // the registrar is reusable after a chain ends (issue #130).
        reg.link_from("mqtt://broker/topic-c")
            .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
        reg.with_name("third-link");

        assert_eq!(rec.inbound_connectors().len(), 3);
    }

    /// Registrar methods take fresh borrows (issue #130): separate
    /// statements in a configure-style closure must compile.
    #[test]
    fn registrar_allows_separate_statements() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> = vec![];
        let extensions = crate::extensions::Extensions::new();
        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.source(|_ctx, _p| async move {});
        reg.tap(|_ctx, _c| async move {});

        assert!(rec.has_producer());
        assert_eq!(rec.consumer_count(), 1);
    }

    // ====================================================================
    // build()-level validation tests (issue #133)
    // ====================================================================

    #[derive(Debug, Clone)]
    struct OtherRecord;

    /// Connector builder whose `build()` contributes nothing — for tests
    /// that must get through the connector phase.
    struct NoopConnectorBuilder;

    impl crate::connector::ConnectorBuilder for NoopConnectorBuilder {
        fn build<'a>(
            &'a self,
            _db: &'a crate::AimDb,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn scheme(&self) -> &str {
            "mqtt"
        }
    }

    /// Acceptance criterion (issue #133): a builder with three distinct
    /// mistakes reports all three from one `build()` call.
    #[tokio::test]
    async fn build_reports_all_configuration_mistakes_at_once() {
        let mut builder = crate::AimDbBuilder::new().runtime(Arc::new(MockRuntime));

        // Mistake 1: outbound link with no serializer
        builder.configure::<TestRecord>("rec.a", |reg| {
            reg.link_to("mqtt://broker/a").finish();
        });
        // Mistake 2: two .source() registrations on one record
        builder.configure::<TestRecord>("rec.b", |reg| {
            reg.source(|_ctx, _p| async move {});
            reg.source(|_ctx, _p| async move {});
        });
        // Mistake 3: key re-registered with a different type
        builder.configure::<TestRecord>("rec.c", |_reg| {});
        builder.configure::<OtherRecord>("rec.c", |_reg| {});

        let Err(err) = builder.build().await else {
            panic!("build must fail");
        };
        let crate::DbError::InvalidConfiguration { errors } = err else {
            panic!("expected InvalidConfiguration, got {err:?}");
        };
        assert_eq!(errors.len(), 3, "expected 3 errors, got: {errors:?}");
        assert!(errors.iter().any(|e| e.record_key == "rec.a"
            && e.url.as_deref() == Some("mqtt://broker/a")
            && e.message.contains("requires a serializer")));
        assert!(errors.iter().any(
            |e| e.record_key == "rec.b" && e.message.contains("already has a producer service")
        ));
        assert!(errors
            .iter()
            .any(|e| e.record_key == "rec.c" && e.message.contains("different type")));
    }

    /// `.buffer()` after `.link_from()` is legitimate now that the buffer
    /// requirement is validated by `build()` instead of `finish()`.
    #[tokio::test]
    async fn buffer_after_link_from_is_valid() {
        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);

        builder.configure::<TestRecord>("rec.x", |reg| {
            reg.link_from("mqtt://broker/x")
                .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
            // Buffer configured AFTER the link — order-independent.
            reg.buffer_raw(Box::new(MockBuffer));
        });

        builder.build().await.expect("build must succeed");
    }

    /// A linked record without a buffer fails at build() — previously this
    /// panicked at spawn time, deep inside a connector factory closure.
    #[tokio::test]
    async fn linked_record_without_buffer_fails_build() {
        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);

        builder.configure::<TestRecord>("rec.x", |reg| {
            reg.link_from("mqtt://broker/x")
                .with_deserializer_raw(|_b: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
        });

        let Err(err) = builder.build().await else {
            panic!("build must fail");
        };
        let crate::DbError::InvalidConfiguration { errors } = err else {
            panic!("expected InvalidConfiguration, got {err:?}");
        };
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("requires a buffer"));
        assert_eq!(errors[0].record_key, "rec.x");
        assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/x"));
    }
}
