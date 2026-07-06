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
//! ```no_run
//! # use aimdb_core::{Producer, RuntimeContext};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! # async fn read_sensor() -> Temperature { Temperature { celsius: 21.0 } }
//! async fn temperature_producer(
//!     ctx: RuntimeContext,
//!     producer: Producer<Temperature>,
//! ) {
//!     loop {
//!         let temp = read_sensor().await;
//!         producer.produce(temp);
//!         ctx.time().sleep_secs(1).await;
//!     }
//! }
//! ```
//!
//! # Consumer Example
//!
//! ```no_run
//! # use aimdb_core::{Consumer, RuntimeContext};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! async fn temperature_monitor(
//!     ctx: RuntimeContext,
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
//! Illustrative (not compiled: `.buffer()` comes from your runtime adapter's
//! registrar extension trait, which `aimdb-core` cannot depend on):
//!
//! ```rust,ignore
//! builder.configure::<Temperature>("sensors.outdoor", |reg| {
//!     reg.buffer(cfg)
//!        .source(temperature_producer)
//!        .tap(temperature_monitor)
//!        .link_to("mqtt://sensors/temp")
//!        .with_serializer(|_ctx, t| serde_json::to_vec(t))
//!        .finish();
//! });
//! ```

use core::fmt::Debug;
use core::future::Future;
use core::marker::PhantomData;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::buffer::{DynBuffer, TryProduceError, WriteHandle};
use crate::typed_record::TypedRecord;
use crate::AimDb;

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
///.
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
    #[cfg(feature = "observability")]
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
            #[cfg(feature = "observability")]
            profiling: None,
            _phantom: PhantomData,
        }
    }

    /// Attaches stage profiling state. Internal — called by the spawn machinery.
    #[cfg(feature = "observability")]
    pub(crate) fn set_profiling(
        &mut self,
        metrics: Arc<crate::profiling::StageMetrics>,
        clock: crate::profiling::Clock,
    ) {
        self.profiling = Some(Arc::new(crate::profiling::ProducerProfilingState::new(
            metrics, clock,
        )));
    }

    /// Push a value. Infallible — overwrite-on-overflow buffers cannot reject.
    /// Use this for fire-and-forget telemetry.
    ///
    /// Forwards the value to the record's buffer, the latest-snapshot slot,
    /// and the metadata tracker in a single synchronous call.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aimdb_core::{Producer, RuntimeContext};
    /// # #[derive(Clone, Debug)] struct Telemetry { celsius: f32 }
    /// # async fn read_sensor() -> Telemetry { Telemetry { celsius: 21.0 } }
    /// async fn sensor_loop(ctx: RuntimeContext, producer: Producer<Telemetry>) {
    ///     loop {
    ///         producer.produce(read_sensor().await);
    ///         ctx.time().sleep_secs(1).await;
    ///     }
    /// }
    /// ```
    pub fn produce(&self, value: T) {
        #[cfg(feature = "observability")]
        if let Some(state) = &self.profiling {
            state.record_produce();
        }
        self.write.push(value);
    }

    /// Non-blocking push. Returns the value back via [`TryProduceError::Full`]
    /// if a bounded buffer is at capacity, or [`TryProduceError::Closed`] if
    /// the record is shutting down. Use when the caller has a meaningful
    /// response to backpressure.
    ///
    /// Overwriting buffers (`SpmcRing`, `SingleLatest`, `Mailbox`) always
    /// return `Ok(())`. Use [`produce`](Self::produce) for those.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aimdb_core::{Producer, TryProduceError};
    /// # #[derive(Clone, Debug)] struct Command { id: u32 }
    /// fn send_command(producer: &Producer<Command>, cmd: Command) {
    ///     match producer.try_produce(cmd) {
    ///         Ok(()) => {}
    ///         Err(TryProduceError::Full(_)) => { /* backpressure: retry later */ }
    ///         Err(TryProduceError::Closed(_)) => { /* record shut down */ }
    ///     }
    /// }
    /// ```
    pub fn try_produce(&self, value: T) -> Result<(), TryProduceError<T>> {
        self.write.try_push(value)
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        Self {
            write: self.write.clone(),
            #[cfg(feature = "observability")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
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
    #[cfg(feature = "observability")]
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
            #[cfg(feature = "observability")]
            profiling: None,
            _phantom: PhantomData,
        }
    }

    /// Attaches stage profiling state. Internal — called by the spawn machinery.
    #[cfg(feature = "observability")]
    pub(crate) fn set_profiling(
        &mut self,
        metrics: Arc<crate::profiling::StageMetrics>,
        clock: crate::profiling::Clock,
    ) {
        self.profiling = Some((metrics, clock));
    }

    /// Subscribe to updates for this record type.
    ///
    /// Returns a [`Reader<T>`](crate::buffer::Reader) that yields values as they
    /// are produced. Its `recv().await` is allocation-free.
    /// Infallible — the buffer is pre-resolved at `Consumer` construction.
    pub fn subscribe(&self) -> crate::buffer::Reader<T> {
        let reader = self.buffer.subscribe_boxed();
        #[cfg(feature = "observability")]
        if let Some((metrics, clock)) = &self.profiling {
            return crate::buffer::Reader::new(Box::new(
                crate::profiling::ProfilingBufferReader::new(
                    reader,
                    metrics.clone(),
                    clock.clone(),
                ),
            ));
        }
        crate::buffer::Reader::new(reader)
    }
}

impl<T> Clone for Consumer<T> {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            #[cfg(feature = "observability")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
    }
}

// ============================================================================
// Fused outbound source
// ============================================================================

/// Type alias for the unified typed serializer captured by [`FusedSource`]
///
/// Raw and context-aware serializers collapse into this shape at `finish()`;
/// the raw variant simply ignores the threaded context.
type FusedSerializeFn<T> = Arc<
    dyn Fn(&crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
        + Send
        + Sync,
>;

/// The [`SerializedSource`](crate::connector::SerializedSource) built by
/// `OutboundConnectorBuilder::finish()` — holds the typed consumer,
/// serializer, and optional topic provider, so every per-message step stays
/// typed (no `Box<dyn Any>`).
struct FusedSource<T: Send + Sync + 'static + Debug + Clone> {
    consumer: Consumer<T>,
    serialize: FusedSerializeFn<T>,
    topic: Option<Arc<dyn crate::connector::TopicProvider<T>>>,
}

impl<T> crate::connector::SerializedSource for FusedSource<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn subscribe(&self) -> Box<dyn crate::connector::SerializedReader> {
        Box::new(FusedReader {
            inner: self.consumer.subscribe(),
            serialize: self.serialize.clone(),
            topic: self.topic.clone(),
        })
    }
}

/// One subscription of a [`FusedSource`]: recv → resolve destination →
/// serialize, all on the typed value.
///
/// The connector SPI keeps its boxed `RecvSerializedFuture` (BYOC stays
/// stable); only the *inner* per-message box is eliminated by reading through
/// the allocation-free [`Reader<T>`](crate::buffer::Reader).
struct FusedReader<T: Clone + Send + 'static> {
    inner: crate::buffer::Reader<T>,
    serialize: FusedSerializeFn<T>,
    topic: Option<Arc<dyn crate::connector::TopicProvider<T>>>,
}

impl<T: Clone + Send + 'static> crate::connector::SerializedReader for FusedReader<T> {
    fn recv<'a>(
        &'a mut self,
        ctx: &'a crate::RuntimeContext,
    ) -> crate::connector::RecvSerializedFuture<'a> {
        Box::pin(async move {
            loop {
                // Buffer errors propagate unchanged: `BufferLagged` lets the
                // pump skip the gap and keep going; anything else ends it.
                let value = self.inner.recv().await?;
                // Resolve the destination while the typed value is in hand.
                let dest = self.topic.as_ref().and_then(|p| p.topic(&value));
                match (self.serialize)(ctx, &value) {
                    Ok(payload) => return Ok(crate::connector::SerializedValue { dest, payload }),
                    Err(_e) => {
                        // Same skip-and-log the pumps used to do around the
                        // erased serializer.
                        log_error!(
                            "outbound link: failed to serialize {} (dest {:?}): {:?}",
                            core::any::type_name::<T>(),
                            dest,
                            _e
                        );
                        continue;
                    }
                }
            }
        })
    }
}

// ============================================================================
// RecordRegistrar - Fluent registration API
// ============================================================================

/// Type alias for typed context-aware serializer callbacks
///
/// Stays typed until `finish()` fuses it with the consumer — no per-message
/// erasure.
type TypedContextSerializerFn<T> = Arc<
    dyn Fn(crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
        + Send
        + Sync
        + 'static,
>;

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
    /// Tracked even when the `observability` feature is off (then it's just unused).
    #[cfg_attr(not(feature = "observability"), allow(dead_code))]
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
    /// available; when the `observability` feature is disabled it is a no-op.
    pub fn with_name(&mut self, name: &str) -> &mut Self {
        #[cfg(feature = "observability")]
        if let Some((kind, idx)) = self.last_stage {
            self.rec.profiling_mut().set_stage_name(kind, idx, name);
        }
        #[cfg(not(feature = "observability"))]
        let _ = name;
        self
    }

    /// Registers a signal gauge on this record and returns a handle to feed it.
    ///
    /// Values pushed via [`SignalGaugeHandle::update`](crate::SignalGaugeHandle::update)
    /// fold into per-record last/min/max/mean statistics that surface on
    /// `record.list` / `record.get` and stage profiling. This is the core hook
    /// behind `Observable::observe()` (design 041 §3.2).
    ///
    /// Always available, mirroring [`with_name`](Self::with_name): when the
    /// `observability` feature is disabled it returns an inert handle whose
    /// `update` is a no-op, so callers never `#[cfg]` on core's features.
    pub fn signal_gauge(
        &mut self,
        name: &'static str,
        unit: &'static str,
    ) -> crate::signal::SignalGaugeHandle {
        #[cfg(feature = "observability")]
        {
            let stats = self.rec.profiling_mut().push_signal_gauge(name, unit);
            crate::signal::SignalGaugeHandle::live(stats)
        }
        #[cfg(not(feature = "observability"))]
        {
            let _ = (name, unit);
            crate::signal::SignalGaugeHandle::inert()
        }
    }

    /// Registers a producer service for this record type.
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext)
    /// (time + logging capabilities) and a pre-resolved [`Producer<T>`]; it is
    /// collected at `build()` time and driven by the `AimDbRunner`.
    pub fn source<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(crate::RuntimeContext, crate::Producer<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.rec.set_producer(f);
        #[cfg(feature = "observability")]
        {
            let (idx, _) = self.rec.profiling_mut().push_source();
            self.last_stage = Some((StageKind::Source, idx));
        }
        #[cfg(not(feature = "observability"))]
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
    pub fn tap<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(crate::RuntimeContext, crate::Consumer<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        T: Sync,
    {
        self.rec.add_consumer(f);
        #[cfg(feature = "observability")]
        {
            let (idx, _) = self.rec.profiling_mut().push_tap();
            self.last_stage = Some((StageKind::Tap, idx));
        }
        #[cfg(not(feature = "observability"))]
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

    /// Installs the JSON codec for this record (feature `remote`)
    ///
    /// Enables `record.latest()?.as_json()`, and on `std` the AimX `record.get`
    /// / `set` / `subscribe` protocol. Requires `T: RemoteSerialize`
    /// (blanket-impl'd for every `Serialize + DeserializeOwned` type). Works on
    /// no_std + alloc.
    #[cfg(feature = "remote")]
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
        #[cfg(feature = "observability")]
        {
            let (idx, _) = self.rec.profiling_mut().push_transform();
            self.last_stage = Some((StageKind::Transform, idx));
        }
        #[cfg(not(feature = "observability"))]
        {
            self.last_stage = Some((StageKind::Transform, 0));
        }
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
        #[cfg(feature = "observability")]
        {
            let (idx, _) = self.rec.profiling_mut().push_transform();
            self.last_stage = Some((StageKind::Transform, idx));
        }
        #[cfg(not(feature = "observability"))]
        {
            self.last_stage = Some((StageKind::Transform, 0));
        }
        self
    }

    /// Link TO external system (outbound: AimDB → External)
    ///
    /// Subscribes to buffer updates and publishes them to an external system.
    pub fn link_to(&mut self, url: &str) -> OutboundConnectorBuilder<'_, 'a, T> {
        OutboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            context_serializer: None,
            topic_provider: None,
        }
    }

    /// Link FROM external system (inbound: External → AimDB)
    ///
    /// Subscribes to an external data source and produces values into this record's buffer.
    pub fn link_from(&mut self, url: &str) -> InboundConnectorBuilder<'_, 'a, T> {
        InboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
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
    context_serializer: Option<TypedContextSerializerFn<T>>,
    topic_provider: Option<Arc<dyn crate::connector::TopicProvider<T>>>,
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

    /// Sets the serialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the typed value being
    /// serialized. Ignore the context parameter (`|_ctx, value| …`) when it is
    /// not needed.
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
            + Send
            + Sync
            + 'static,
    {
        self.context_serializer = Some(Arc::new(f));
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
    /// The provider is type-checked at compile time against `T` and stays
    /// typed end-to-end: it is fused into the link's serialized source and
    /// called with `&T` per value.
    pub fn with_topic_provider<P>(mut self, provider: P) -> Self
    where
        P: crate::connector::TopicProvider<T> + 'static,
    {
        // Stays typed: fused into the link's SerializedSource at finish().
        self.topic_provider = Some(Arc::new(provider));
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
        use crate::connector::{ConnectorLink, LinkAddress};
        use crate::error::ConfigError;

        let record_key = self.registrar.record_key.clone();

        let Ok(url) = LinkAddress::parse(&self.url) else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Invalid connector URL",
            ));
            return self.registrar;
        };

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        // Adapt the stored serializer to the fused calling convention. Stays
        // typed: fused with the consumer below, no `Box<dyn Any>` per message
        //.
        let serialize: FusedSerializeFn<T> = if let Some(ser) = self.context_serializer {
            Arc::new(move |ctx: &crate::RuntimeContext, value: &T| ser(ctx.clone(), value))
        } else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Outbound connector requires a serializer. Call .with_serializer()",
            ));
            return self.registrar;
        };

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
        #[cfg(feature = "observability")]
        let link_metrics = {
            let (idx, metrics) = self.registrar.rec.profiling_mut().push_link();
            self.registrar.last_stage = Some((StageKind::Link, idx));
            metrics
        };
        #[cfg(not(feature = "observability"))]
        {
            self.registrar.last_stage = Some((StageKind::Link, 0));
        }

        // Fused source factory that captures type T and record key.
        //
        // Resolves the record at route-collection time (not per-message) and
        // constructs a `Consumer<T>` bound to a pre-resolved buffer handle —
        // same pattern as the build-time path in
        // `TypedRecord::collect_consumer_futures`. The serializer
        // and topic provider ride along typed, so the readers handed to the
        // pumps yield destination + payload with no erasure crossing.
        //
        // The factory runs during build() after every record is registered and
        // validated (including the linked-records-need-a-buffer check), so
        // failures here are aimdb bugs, not user mistakes.
        let source_factory: crate::connector::SourceFactoryFn = {
            let record_key = self.registrar.record_key.clone();
            let topic_provider = self.topic_provider;
            Arc::new(move |db: &AimDb| {
                let typed_rec = db
                    .inner()
                    .get_typed_record_by_key::<T>(&record_key)
                    .unwrap_or_else(|e| {
                        panic!(
                            "source factory: record '{record_key}' lookup failed ({e:?}) — \
                             this is a bug in aimdb-core"
                        )
                    });
                let buffer = typed_rec.buffer_handle().unwrap_or_else(|| {
                    panic!(
                        "source factory: record '{record_key}' has no buffer despite \
                         build()-time validation — this is a bug in aimdb-core"
                    )
                });

                #[allow(unused_mut)]
                let mut consumer = Consumer::<T>::new(buffer);
                #[cfg(feature = "observability")]
                consumer.set_profiling(link_metrics.clone(), db.profiling_clock().clone());
                Box::new(FusedSource {
                    consumer,
                    serialize: serialize.clone(),
                    topic: topic_provider.clone(),
                }) as Box<dyn crate::connector::SerializedSource>
            })
        };

        let mut link = ConnectorLink::new(url, source_factory);
        link.config = self.config;

        // Store the connector link - sources will be created later in build()
        // after connectors are actually built
        self.registrar.rec.add_outbound_connector(link);
        self.registrar
    }
}

// ============================================================================
// InboundConnectorBuilder - Fluent inbound connector configuration
// ============================================================================

/// Type alias for typed context-aware deserializer callbacks
///
/// Stays typed until `finish()` fuses it with the producer — no per-message
/// erasure.
type TypedContextDeserializerFn<T> =
    Arc<dyn Fn(crate::RuntimeContext, &[u8]) -> Result<T, String> + Send + Sync + 'static>;

/// Builder for configuring inbound connector links (External → AimDB)
///
/// `'r` is the borrow of the registrar taken by `link_from()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub struct InboundConnectorBuilder<'r, 'a, T: Send + Sync + 'static + Debug + Clone> {
    registrar: &'r mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    context_deserializer: Option<TypedContextDeserializerFn<T>>,
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

    /// Sets the deserialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the raw bytes from
    /// the external system. Ignore the context parameter (`|_ctx, data| …`)
    /// when it is not needed.
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        self.context_deserializer = Some(Arc::new(f));
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
        use crate::connector::{InboundConnectorLink, LinkAddress};
        use crate::error::ConfigError;

        let record_key = self.registrar.record_key.clone();

        let Ok(url) = LinkAddress::parse(&self.url) else {
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

        // Mutual exclusion with local producers (.source()/.transform()) is
        // validated once, in build(), where the record key is known.

        // Adapt the stored deserializer to the fused calling convention. Stays
        // typed: fused with the producer below, no `Box<dyn Any>` per message
        //.
        type UnifiedDeserializeFn<T> =
            Arc<dyn Fn(&crate::RuntimeContext, &[u8]) -> Result<T, String> + Send + Sync>;
        let deserialize: UnifiedDeserializeFn<T> = if let Some(deser) = self.context_deserializer {
            Arc::new(move |ctx, bytes| deser(ctx.clone(), bytes))
        } else {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Inbound connector requires a deserializer. Call .with_deserializer()",
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

        // Fused ingest factory that captures type T and record key: resolves
        // the typed producer once at route-collection time; per message the
        // returned IngestFn runs deserialize + produce with no erasure
        // crossing. The factory runs during build() after every record is
        // registered and validated, so failures here are aimdb bugs, not
        // user mistakes.
        let ingest_factory: crate::connector::IngestFactoryFn = {
            let record_key = self.registrar.record_key.clone();
            Arc::new(move |db: &AimDb| {
                let typed_rec = db
                    .inner()
                    .get_typed_record_by_key::<T>(&record_key)
                    .unwrap_or_else(|e| {
                        panic!(
                            "ingest factory: record '{record_key}' lookup failed ({e:?}) — \
                             this is a bug in aimdb-core"
                        )
                    });
                let producer = Producer::<T>::new(typed_rec.writer_handle());
                let deserialize = deserialize.clone();
                Arc::new(move |ctx: &crate::RuntimeContext, payload: &[u8]| {
                    producer.produce(deserialize(ctx, payload)?);
                    Ok(())
                }) as crate::connector::IngestFn
            })
        };

        // Create inbound connector link
        let mut link = InboundConnectorLink::new(url, ingest_factory);
        link.config = self.config;

        // Wire through the topic resolver
        link.topic_resolver = self.topic_resolver;

        // Add to record
        self.registrar.rec.add_inbound_connector(link);
        self.registrar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbResult;
    use core::pin::Pin;

    #[cfg(not(feature = "std"))]
    use alloc::vec;

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    struct TestRecord {
        value: i32,
    }

    #[test]
    fn test_link_address_simple() {
        use crate::connector::LinkAddress;
        let addr = LinkAddress::parse("mqtt://sensors").unwrap();
        assert_eq!(addr.scheme(), "mqtt");
        assert_eq!(addr.resource_id(), "sensors");
    }

    #[test]
    fn test_link_address_multi_level() {
        use crate::connector::LinkAddress;
        let addr = LinkAddress::parse("mqtt://sensors/temperature").unwrap();
        assert_eq!(addr.resource_id(), "sensors/temperature");
    }

    #[test]
    fn test_link_address_deep() {
        use crate::connector::LinkAddress;
        let addr = LinkAddress::parse("mqtt://factory/floor1/sensors/temp").unwrap();
        assert_eq!(addr.resource_id(), "factory/floor1/sensors/temp");
    }

    // ====================================================================
    // Test infrastructure for InboundConnectorBuilder deserializer tests
    // ====================================================================

    /// Minimal mock runtime for context tests — the builder only needs the
    /// dyn-safe `RuntimeOps` surface, supplied by the shared test stub.
    use crate::executor::test_support::NoopRuntimeOps as MockRuntime;

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
    // Inbound link registration tests (fused ingest)
    // ====================================================================

    #[test]
    fn inbound_finish_registers_fused_link() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_from("mqtt://broker/topic")
            .with_deserializer(|_ctx, bytes: &[u8]| {
                Ok(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();

        assert_eq!(rec.inbound_connectors().len(), 1);
        assert_eq!(rec.inbound_connectors()[0].resolve_topic(), "broker/topic");
        assert!(drain_errors(&mut rec).is_empty());
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
    // Outbound link registration tests (fused source)
    // ====================================================================

    #[test]
    fn outbound_finish_registers_fused_link() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();

        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_to("mqtt://broker/topic")
            .with_serializer(|_ctx, record: &TestRecord| Ok(record.value.to_le_bytes().to_vec()))
            .finish();

        assert_eq!(rec.outbound_connectors().len(), 1);
        assert_eq!(
            rec.outbound_connectors()[0].url.resource_id(),
            "broker/topic"
        );
        assert!(drain_errors(&mut rec).is_empty());
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
            .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
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
            build_fn: Box::new(|_p, _db, _output_key, _profiling| {
                crate::transform::CollectedTransform {
                    task_future: Box::pin(async {}),
                    fanin_futures: vec![],
                }
            }),
        }
    }

    #[test]
    fn cross_stage_registrations_record_without_setter_errors() {
        // Cross-stage exclusivity (.source()/.transform()/.link_from()) is
        // validated by build(), not by the setters: conflicting registrations
        // are all recorded so build() can report the conflict with the record
        // key attached.
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
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();

        assert!(rec.has_producer());
        assert_eq!(rec.inbound_connectors().len(), 1);
        assert!(
            drain_errors(&mut rec).is_empty(),
            "no setter-level cross-stage errors; build() reports the conflict"
        );
    }

    #[test]
    fn duplicate_source_and_transform_record_errors() {
        // Same-stage duplicates are still caught at registration time: a
        // second .source()/.transform() would silently overwrite the first.
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));

        rec.set_producer(|_ctx, _p| async move {});
        rec.set_producer(|_ctx, _p| async move {});
        rec.set_transform(dummy_transform_descriptor());
        rec.set_transform(dummy_transform_descriptor());

        let errors = drain_errors(&mut rec);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].message.contains("already has a producer service"));
        assert!(errors[1]
            .message
            .contains("already has a .transform(); only one is allowed"));
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
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish()
            .link_from("mqtt://broker/topic-b")
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();

        // … and as separate statements: each call takes a fresh borrow, so
        // the registrar is reusable after a chain ends.
        reg.link_from("mqtt://broker/topic-c")
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
        reg.with_name("third-link");

        assert_eq!(rec.inbound_connectors().len(), 3);
    }

    /// Registrar methods take fresh borrows: separate
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
    // build()-level validation tests
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

    /// Acceptance criterion: a builder with three distinct
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
                .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
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
                .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
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

    // ====================================================================
    // Fused ingest roundtrip tests
    // ====================================================================

    use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    /// Buffer that records the last pushed value and a push count —
    /// atomics only, so the test runs under no_std + alloc too.
    struct RecordingBuffer {
        last: Arc<AtomicI32>,
        count: Arc<AtomicUsize>,
    }

    impl crate::buffer::DynBuffer<TestRecord> for RecordingBuffer {
        fn push(&self, value: TestRecord) {
            self.last.store(value.value, Ordering::SeqCst);
            self.count.fetch_add(1, Ordering::SeqCst);
        }
        fn subscribe_boxed(&self) -> Box<dyn crate::buffer::BufferReader<TestRecord> + Send> {
            unimplemented!("not needed for ingest tests")
        }
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    /// End-to-end inbound path: bytes → fused ingest → typed buffer push,
    /// with no `Box<dyn Any>` in between.
    #[tokio::test]
    async fn ingest_roundtrip_produces_value() {
        let last = Arc::new(AtomicI32::new(-1));
        let count = Arc::new(AtomicUsize::new(0));
        let (buf_last, buf_count) = (last.clone(), count.clone());

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);
        builder.configure::<TestRecord>("rec.in", move |reg| {
            reg.buffer_raw(Box::new(RecordingBuffer {
                last: buf_last,
                count: buf_count,
            }));
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, bytes: &[u8]| {
                    if bytes.is_empty() {
                        return Err("empty payload".to_string());
                    }
                    Ok(TestRecord {
                        value: bytes.len() as i32,
                    })
                })
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_inbound_routes("mqtt");
        assert_eq!(routes.len(), 1);
        let (topic, ingest) = &routes[0];
        assert_eq!(topic, "cmd/in");

        let ctx = db.runtime_ctx();
        ingest(&ctx, &[1, 2, 3]).expect("ingest must succeed");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(last.load(Ordering::SeqCst), 3);

        // Bad bytes: the deserializer error propagates, nothing is produced.
        let err = ingest(&ctx, &[]).expect_err("empty payload must fail");
        assert_eq!(err, "empty payload");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// Raw/context mutual exclusion is behavior now (the kind enum is gone):
    /// the variant set last wins inside the fused ingest closure.
    #[tokio::test]
    async fn context_deserializer_set_last_wins() {
        let last = Arc::new(AtomicI32::new(-1));
        let count = Arc::new(AtomicUsize::new(0));
        let (buf_last, buf_count) = (last.clone(), count.clone());

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);
        builder.configure::<TestRecord>("rec.in", move |reg| {
            reg.buffer_raw(Box::new(RecordingBuffer {
                last: buf_last,
                count: buf_count,
            }));
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                    Ok(TestRecord { value: 99 })
                })
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_inbound_routes("mqtt");
        let (_, ingest) = &routes[0];
        ingest(&db.runtime_ctx(), b"x").expect("ingest must succeed");
        assert_eq!(last.load(Ordering::SeqCst), 99);
    }

    /// And the reverse: raw set last wins over a prior context deserializer.
    #[tokio::test]
    async fn raw_deserializer_set_last_wins() {
        let last = Arc::new(AtomicI32::new(-1));
        let count = Arc::new(AtomicUsize::new(0));
        let (buf_last, buf_count) = (last.clone(), count.clone());

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);
        builder.configure::<TestRecord>("rec.in", move |reg| {
            reg.buffer_raw(Box::new(RecordingBuffer {
                last: buf_last,
                count: buf_count,
            }));
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                    Ok(TestRecord { value: 0 })
                })
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 7 }))
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_inbound_routes("mqtt");
        let (_, ingest) = &routes[0];
        ingest(&db.runtime_ctx(), b"x").expect("ingest must succeed");
        assert_eq!(last.load(Ordering::SeqCst), 7);
    }

    // ====================================================================
    // Fused outbound reader tests
    // ====================================================================

    use crate::connector::SerializedReader as _;

    /// Buffer reader that replays a fixed script, then reports the buffer
    /// closed.
    struct ScriptedReader {
        script: Vec<Result<TestRecord, crate::DbError>>,
    }

    impl ScriptedReader {
        fn closed() -> crate::DbError {
            crate::DbError::BufferClosed {
                buffer_name: "scripted".to_string(),
            }
        }
    }

    impl crate::buffer::BufferReader<TestRecord> for ScriptedReader {
        fn poll_recv(
            &mut self,
            _cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Result<TestRecord, crate::DbError>> {
            let next = if self.script.is_empty() {
                Err(Self::closed())
            } else {
                self.script.remove(0)
            };
            core::task::Poll::Ready(next)
        }
        fn try_recv(&mut self) -> Result<TestRecord, crate::DbError> {
            unimplemented!("not needed for fused reader tests")
        }
    }

    fn lagged() -> crate::DbError {
        crate::DbError::BufferLagged {
            lag_count: 1,
            buffer_name: "scripted".to_string(),
        }
    }

    fn fused_reader(
        script: Vec<Result<TestRecord, crate::DbError>>,
        serialize: FusedSerializeFn<TestRecord>,
        topic: Option<Arc<dyn crate::connector::TopicProvider<TestRecord>>>,
    ) -> FusedReader<TestRecord> {
        FusedReader {
            inner: crate::buffer::Reader::new(Box::new(ScriptedReader { script })),
            serialize,
            topic,
        }
    }

    fn test_ctx() -> crate::RuntimeContext {
        crate::RuntimeContext::new(Arc::new(MockRuntime))
    }

    /// Buffer errors propagate through the fused reader unchanged, so the
    /// pumps keep their `BufferLagged => continue / Err => break` shape.
    #[tokio::test]
    async fn fused_reader_propagates_buffer_errors() {
        let mut reader = fused_reader(
            vec![
                Ok(TestRecord { value: 1 }),
                Err(lagged()),
                Ok(TestRecord { value: 2 }),
            ],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            None,
        );
        let ctx = test_ctx();

        let first = reader.recv(&ctx).await.expect("first value");
        assert_eq!(first.payload, 1i32.to_le_bytes().to_vec());
        assert_eq!(first.dest, None);

        let err = reader.recv(&ctx).await.expect_err("lag must propagate");
        assert!(matches!(err, crate::DbError::BufferLagged { .. }));

        let second = reader.recv(&ctx).await.expect("second value");
        assert_eq!(second.payload, 2i32.to_le_bytes().to_vec());

        let closed = reader.recv(&ctx).await.expect_err("closed must propagate");
        assert!(matches!(closed, crate::DbError::BufferClosed { .. }));
    }

    /// Serialization failures are skipped inside the reader (logged), exactly
    /// like the old pump-side `continue`.
    #[tokio::test]
    async fn fused_reader_skips_serialize_failures() {
        let mut reader = fused_reader(
            vec![Ok(TestRecord { value: 13 }), Ok(TestRecord { value: 42 })],
            Arc::new(|_ctx, r| {
                if r.value == 13 {
                    Err(crate::connector::SerializeError::InvalidData)
                } else {
                    Ok(r.value.to_le_bytes().to_vec())
                }
            }),
            None,
        );

        // One recv: the failing value is skipped, the next good one returned.
        let msg = reader.recv(&test_ctx()).await.expect("value");
        assert_eq!(msg.payload, 42i32.to_le_bytes().to_vec());
    }

    /// The destination is resolved from the typed value while it is in hand.
    #[tokio::test]
    async fn fused_reader_resolves_dynamic_topic() {
        struct PositiveTopic;
        impl crate::connector::TopicProvider<TestRecord> for PositiveTopic {
            fn topic(&self, value: &TestRecord) -> Option<String> {
                (value.value > 0).then(|| alloc::format!("dyn/{}", value.value))
            }
        }

        let mut reader = fused_reader(
            vec![Ok(TestRecord { value: 5 }), Ok(TestRecord { value: 0 })],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            Some(Arc::new(PositiveTopic)),
        );
        let ctx = test_ctx();

        let first = reader.recv(&ctx).await.expect("value");
        assert_eq!(first.dest.as_deref(), Some("dyn/5"));

        let second = reader.recv(&ctx).await.expect("value");
        assert_eq!(second.dest, None); // falls back to the route default
    }

    /// End-to-end outbound path: registrar → build → collect → subscribe →
    /// recv, pinning the factory wiring (raw and context serializers).
    #[tokio::test]
    async fn outbound_roundtrip_yields_serialized_values() {
        /// Buffer whose readers replay one canned value, then close.
        struct CannedBuffer;
        impl crate::buffer::DynBuffer<TestRecord> for CannedBuffer {
            fn push(&self, _value: TestRecord) {}
            fn subscribe_boxed(&self) -> Box<dyn crate::buffer::BufferReader<TestRecord> + Send> {
                Box::new(ScriptedReader {
                    script: vec![Ok(TestRecord { value: 5 })],
                })
            }
            fn as_any(&self) -> &dyn core::any::Any {
                self
            }
        }

        struct FixedTopic;
        impl crate::connector::TopicProvider<TestRecord> for FixedTopic {
            fn topic(&self, value: &TestRecord) -> Option<String> {
                Some(alloc::format!("dyn/{}", value.value))
            }
        }

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);
        builder.configure::<TestRecord>("rec.out", |reg| {
            reg.buffer_raw(Box::new(CannedBuffer));
            // Raw set first, context set last — context must win (the kind
            // enum is gone; mutual exclusion is behavior now).
            reg.link_to("mqtt://tele/out")
                .with_topic_provider(FixedTopic)
                .with_serializer(|_ctx, _r: &TestRecord| Ok(vec![0]))
                .with_serializer(|_ctx: crate::RuntimeContext, r: &TestRecord| {
                    Ok(r.value.to_le_bytes().to_vec())
                })
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_outbound_routes("mqtt");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].topic, "tele/out");

        let mut reader = routes[0].source.subscribe();
        let ctx = db.runtime_ctx();
        let msg = reader.recv(&ctx).await.expect("value");
        assert_eq!(msg.dest.as_deref(), Some("dyn/5"));
        assert_eq!(msg.payload, 5i32.to_le_bytes().to_vec());

        let closed = reader.recv(&ctx).await.expect_err("buffer closed");
        assert!(matches!(closed, crate::DbError::BufferClosed { .. }));
    }
}
