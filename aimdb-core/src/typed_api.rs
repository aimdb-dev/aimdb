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

/// Builds a link's `Consumer<T>` from the live database, once per route.
type ConsumerFactoryFn<T> = Arc<dyn Fn(&AimDb) -> Consumer<T> + Send + Sync>;

/// Optional allocation-free serializer captured beside [`FusedSerializeFn`].
///
/// The callback writes into one pump-owned bounded scratch buffer. Returning
/// `BufferTooSmall` selects the owned serializer for that value; other failures
/// retain the existing skip-and-log behavior.
type FusedSerializeIntoFn<T> = Arc<
    dyn Fn(&crate::RuntimeContext, &T, &mut [u8]) -> Result<usize, crate::connector::SerializeError>
        + Send
        + Sync,
>;

/// How an outbound link picks each value's destination. Setting one replaces
/// the other.
enum TopicSelector<T> {
    None,
    Provider(Arc<dyn crate::connector::TopicProvider<T>>),
    Writer {
        capacity: usize,
        writer: Arc<dyn crate::connector::TopicWriter<T>>,
    },
}

impl<T> Clone for TopicSelector<T> {
    fn clone(&self) -> Self {
        match self {
            Self::None => Self::None,
            Self::Provider(p) => Self::Provider(p.clone()),
            Self::Writer { capacity, writer } => Self::Writer {
                capacity: *capacity,
                writer: writer.clone(),
            },
        }
    }
}

/// The [`SerializedSource`](crate::connector::SerializedSource) built by
/// `OutboundConnectorBuilder::finish()` — holds the typed consumer,
/// serializer, and optional topic provider, so every per-message step stays
/// typed (no `Box<dyn Any>`).
struct FusedSource<T: Send + Sync + 'static + Debug + Clone> {
    consumer: Consumer<T>,
    serialize: FusedSerializeFn<T>,
    serialize_into: Option<(usize, FusedSerializeIntoFn<T>)>,
    topic: TopicSelector<T>,
}

impl<T> crate::connector::SerializedSource for FusedSource<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn serializer_scratch_capacity(&self) -> Option<usize> {
        self.serialize_into.as_ref().map(|(capacity, _)| *capacity)
    }

    fn subscribe(&self) -> Box<dyn crate::connector::SerializedReader> {
        let topic_capacity = match &self.topic {
            TopicSelector::Writer { capacity, .. } => *capacity,
            _ => 0,
        };
        Box::new(FusedReader {
            inner: self.consumer.subscribe(),
            serialize: self.serialize.clone(),
            serialize_into: self
                .serialize_into
                .as_ref()
                .map(|(_, serialize_into)| serialize_into.clone()),
            topic: self.topic.clone(),
            topic_buf: alloc::vec![0; topic_capacity].into_boxed_slice(),
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
    serialize_into: Option<FusedSerializeIntoFn<T>>,
    topic: TopicSelector<T>,
    /// Storage a [`TopicWriter`](crate::connector::TopicWriter) writes into;
    /// empty without one.
    topic_buf: Box<[u8]>,
}

impl<T: Clone + Send + 'static> FusedReader<T> {
    /// The value's destination (`None`: the route's default), or `Err` when
    /// its written topic overflowed and the value must be skipped.
    fn resolve_dest(&mut self, value: &T) -> Result<Option<String>, ()> {
        match &self.topic {
            TopicSelector::None => Ok(None),
            TopicSelector::Provider(p) => Ok(p.topic(value)),
            TopicSelector::Writer { writer, .. } => {
                let mut out = crate::connector::TopicBuf::new(&mut self.topic_buf);
                match writer.write_topic(value, &mut out) {
                    Ok(true) if !out.overflowed() => Ok(Some(out.as_str().to_string())),
                    Ok(false) if !out.overflowed() => Ok(None),
                    _ => {
                        log_warn!(
                            "outbound link: topic for {} does not fit in {} bytes, value skipped",
                            core::any::type_name::<T>(),
                            out.capacity()
                        );
                        Err(())
                    }
                }
            }
        }
    }
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
                let Ok(dest) = self.resolve_dest(&value) else {
                    continue;
                };
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

    fn recv_into<'a>(
        &'a mut self,
        ctx: &'a crate::RuntimeContext,
        scratch: &'a mut [u8],
    ) -> crate::connector::RecvSerializedIntoFuture<'a> {
        Box::pin(async move {
            loop {
                let value = self.inner.recv().await?;
                let Ok(dest) = self.resolve_dest(&value) else {
                    continue;
                };

                let Some(serialize_into) = &self.serialize_into else {
                    match (self.serialize)(ctx, &value) {
                        Ok(payload) => {
                            return Ok(crate::connector::SerializedValueInto {
                                dest,
                                payload: crate::connector::SerializedPayload::Owned(payload),
                            });
                        }
                        Err(_e) => {
                            log_error!(
                                "outbound link: failed to serialize {} (dest {:?}): {:?}",
                                core::any::type_name::<T>(),
                                dest,
                                _e
                            );
                            continue;
                        }
                    }
                };

                match serialize_into(ctx, &value, scratch) {
                    Ok(len) => {
                        if scratch.get(..len).is_none() {
                            log_error!(
                                "outbound link: serializer for {} returned invalid length {} for {}-byte scratch buffer",
                                core::any::type_name::<T>(),
                                len,
                                scratch.len()
                            );
                            continue;
                        }
                        return Ok(crate::connector::SerializedValueInto {
                            dest,
                            payload: crate::connector::SerializedPayload::Scratch { len },
                        });
                    }
                    Err(crate::connector::SerializeError::BufferTooSmall) => {
                        match (self.serialize)(ctx, &value) {
                            Ok(payload) => {
                                return Ok(crate::connector::SerializedValueInto {
                                    dest,
                                    payload: crate::connector::SerializedPayload::Owned(payload),
                                });
                            }
                            Err(_e) => {
                                log_error!(
                                    "outbound link: fallback serialization failed for {} (dest {:?}): {:?}",
                                    core::any::type_name::<T>(),
                                    dest,
                                    _e
                                );
                                continue;
                            }
                        }
                    }
                    Err(_e) => {
                        log_error!(
                            "outbound link: failed to serialize {} into scratch buffer (dest {:?}): {:?}",
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

/// One outbound link's per-route state inside
/// [`OutboundRoutes`](crate::OutboundRoutes): reader, topic writer and
/// serializers, all typed.
struct TypedRoute<T: Clone + Send + 'static> {
    reader: crate::buffer::Reader<T>,
    writer: Option<Arc<dyn crate::connector::TopicWriter<T>>>,
    serialize: FusedSerializeFn<T>,
    serialize_into: Option<FusedSerializeIntoFn<T>>,
}

impl<T: Clone + Send + 'static> crate::outbound::PollRoute for TypedRoute<T> {
    fn poll_route(
        &mut self,
        cx: &mut core::task::Context<'_>,
        ctx: &crate::RuntimeContext,
        topic: &mut [u8],
        payload: &mut [u8],
    ) -> core::task::Poll<crate::outbound::RouteOutcome> {
        use crate::outbound::{RouteOutcome, StagedPayload};
        use core::task::Poll;

        let value = match self.reader.poll_recv(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(value)) => value,
            Poll::Ready(Err(crate::DbError::BufferLagged { lag_count, .. })) => {
                return Poll::Ready(RouteOutcome::Lagged(lag_count));
            }
            Poll::Ready(Err(_)) => return Poll::Ready(RouteOutcome::Closed),
        };

        let topic_len = match &self.writer {
            None => None,
            Some(writer) => {
                let mut out = crate::connector::TopicBuf::new(topic);
                match writer.write_topic(&value, &mut out) {
                    Ok(true) if !out.overflowed() => Some(out.len()),
                    Ok(false) if !out.overflowed() => None,
                    _ => return Poll::Ready(RouteOutcome::TopicOverflow),
                }
            }
        };

        let owned = |value: &T| match (self.serialize)(ctx, value) {
            Ok(bytes) => Some(StagedPayload::Owned(bytes)),
            Err(_e) => {
                log_error!(
                    "outbound link: failed to serialize {}: {:?}",
                    core::any::type_name::<T>(),
                    _e
                );
                None
            }
        };
        let staged = match &self.serialize_into {
            None => owned(&value),
            Some(serialize_into) => match serialize_into(ctx, &value, payload) {
                Ok(len) if len <= payload.len() => Some(StagedPayload::Scratch(len)),
                Ok(_len) => {
                    log_error!(
                        "outbound link: serializer for {} returned invalid length {} for {}-byte scratch",
                        core::any::type_name::<T>(),
                        _len,
                        payload.len()
                    );
                    None
                }
                Err(crate::connector::SerializeError::BufferTooSmall) => owned(&value),
                Err(_e) => {
                    log_error!(
                        "outbound link: failed to serialize {} into scratch: {:?}",
                        core::any::type_name::<T>(),
                        _e
                    );
                    None
                }
            },
        };
        Poll::Ready(match staged {
            Some(payload) => RouteOutcome::Staged { topic_len, payload },
            None => RouteOutcome::SerializeFailed,
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

/// Typed into-slice serializer stored by [`OutboundConnectorBuilder`].
type TypedContextSerializerIntoFn<T> = Arc<
    dyn Fn(crate::RuntimeContext, &T, &mut [u8]) -> Result<usize, crate::connector::SerializeError>
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
    /// behind `aimdb-data-contracts`' `Observable::observe()`.
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
    /// Enables `record.latest()?.as_json()` and the AimX `record.get` / `set` /
    /// `subscribe` protocol. Requires `T: RemoteSerialize`
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
            context_serializer_into: None,
            topic: TopicSelector::None,
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
            match_deserializer: None,
            key: None,
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
    context_serializer_into: Option<(usize, TypedContextSerializerIntoFn<T>)>,
    topic: TopicSelector<T>,
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

    /// Adds an into-slice fast path beside the required owned serializer.
    ///
    /// One zero-initialized scratch buffer of `capacity` bytes is allocated when
    /// each route pump starts, then reused for every message. A successful
    /// callback returns the initialized prefix length. The framework validates
    /// that length before borrowing the prefix. `BufferTooSmall` falls back to
    /// [`with_serializer`](Self::with_serializer) for that value; any other
    /// codec error is logged and the value is skipped.
    ///
    /// This method is an optimization only: callers must still install the
    /// owned serializer so oversized and legacy values retain their old behavior.
    pub fn with_serializer_into<F>(mut self, capacity: usize, f: F) -> Self
    where
        F: Fn(
                crate::RuntimeContext,
                &T,
                &mut [u8],
            ) -> Result<usize, crate::connector::SerializeError>
            + Send
            + Sync
            + 'static,
    {
        self.context_serializer_into = Some((capacity, Arc::new(f)));
        self
    }

    /// Removes a previously installed into-slice serializer.
    ///
    /// Higher-level builder extensions use this when replacing a bounded
    /// serialization strategy with an owned-only one. Clearing the fast path
    /// prevents the old scratch serializer from emitting a different wire
    /// format beside the replacement owned serializer.
    pub fn clear_serializer_into(mut self) -> Self {
        self.context_serializer_into = None;
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
        self.topic = TopicSelector::Provider(Arc::new(provider));
        self
    }

    /// Sets a [`TopicWriter`](crate::connector::TopicWriter) that writes each
    /// value's destination.
    ///
    /// `capacity` is the longest topic the writer produces, in bytes. A value
    /// whose topic does not fit is skipped and logged. For a closure, use
    /// [`with_topic_fn`](Self::with_topic_fn).
    pub fn with_topic_writer<W>(mut self, capacity: usize, writer: W) -> Self
    where
        W: crate::connector::TopicWriter<T> + 'static,
    {
        self.topic = TopicSelector::Writer {
            capacity,
            writer: Arc::new(writer),
        };
        self
    }

    /// Sets a closure that writes each value's destination; see
    /// [`with_topic_writer`](Self::with_topic_writer).
    ///
    /// ```rust,ignore
    /// .with_topic_fn(32, |v, out| {
    ///     write!(out, "sensors/{}/{}", v.site, v.id)?;
    ///     Ok(true)
    /// })
    /// ```
    pub fn with_topic_fn<F>(self, capacity: usize, f: F) -> Self
    where
        F: Fn(
                &T,
                &mut crate::connector::TopicBuf<'_>,
            ) -> Result<bool, crate::connector::TopicOverflow>
            + Send
            + Sync
            + 'static,
    {
        self.with_topic_writer(capacity, f)
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

        if url.resource_id().contains(['{', '}']) {
            self.registrar.rec.push_config_error(ConfigError::new(
                record_key,
                Some(self.url),
                "Outbound topics cannot contain '{' or '}'",
            ));
            return self.registrar;
        }

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

        let serialize_into: Option<(usize, FusedSerializeIntoFn<T>)> =
            self.context_serializer_into.map(|(capacity, ser)| {
                let adapted: FusedSerializeIntoFn<T> = Arc::new(
                    move |ctx: &crate::RuntimeContext, value: &T, out: &mut [u8]| {
                        ser(ctx.clone(), value, out)
                    },
                );
                (capacity, adapted)
            });

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

        // Resolves the record and builds a `Consumer<T>` bound to its buffer
        // handle, once per route (not per message) — same pattern as the
        // build-time path in `TypedRecord::collect_consumer_futures`.
        //
        // The factories run during build() after every record is registered
        // and validated (including the linked-records-need-a-buffer check),
        // so failures here are aimdb bugs, not user mistakes.
        #[allow(
            clippy::panic,
            reason = "the factory returns no Result and these lookups were validated at build() time"
        )]
        let make_consumer: ConsumerFactoryFn<T> = {
            let record_key = self.registrar.record_key.clone();
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
                consumer
            })
        };

        // Fused source for the pumps: the serializer and topic selector ride
        // along typed, so its readers yield destination + payload with no
        // erasure crossing.
        let source_factory: crate::connector::SourceFactoryFn = {
            let make_consumer = make_consumer.clone();
            let (serialize, serialize_into, topic) = (
                serialize.clone(),
                serialize_into.clone(),
                self.topic.clone(),
            );
            Arc::new(move |db: &AimDb| {
                Box::new(FusedSource {
                    consumer: make_consumer(db),
                    serialize: serialize.clone(),
                    serialize_into: serialize_into.clone(),
                    topic: topic.clone(),
                }) as Box<dyn crate::connector::SerializedSource>
            })
        };

        // The same parts for `OutboundRoutes`, subscribed when it is built.
        let route_factory: crate::outbound::RouteFactoryFn = {
            let topic = self.topic;
            Arc::new(move |db: &AimDb| {
                let (writer, topic_capacity) = match &topic {
                    TopicSelector::Writer { capacity, writer } => (Some(writer.clone()), *capacity),
                    _ => (None, 0),
                };
                crate::outbound::RouteParts {
                    route: Box::new(TypedRoute {
                        reader: make_consumer(db).subscribe(),
                        writer,
                        serialize: serialize.clone(),
                        serialize_into: serialize_into.as_ref().map(|(_, f)| f.clone()),
                    }),
                    topic_capacity,
                    payload_capacity: serialize_into.as_ref().map_or(0, |(capacity, _)| *capacity),
                    topic_provider: matches!(topic, TopicSelector::Provider(_)),
                }
            })
        };

        let mut link = ConnectorLink::new(url, source_factory);
        link.config = self.config;
        link.route_factory = Some(route_factory);

        // Store the connector link - sources will be created later in build()
        // after connectors are actually built
        self.registrar.rec.add_outbound_connector(link);
        self.registrar
    }
}

// ============================================================================
// InboundConnectorBuilder - Fluent inbound connector configuration
// ============================================================================

/// The producer an inbound ingest factory writes to. Factories run during
/// build() after every record is registered and validated, so a failed lookup
/// is an aimdb bug, not a user mistake.
#[allow(
    clippy::panic,
    reason = "the factory returns no Result and this lookup was validated at build() time"
)]
fn inbound_producer<T>(db: &AimDb, record_key: &str) -> Producer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    let typed_rec = db
        .inner()
        .get_typed_record_by_key::<T>(record_key)
        .unwrap_or_else(|e| {
            panic!(
                "ingest factory: record '{record_key}' lookup failed ({e:?}) — \
                 this is a bug in aimdb-core"
            )
        });
    Producer::<T>::new(typed_rec.writer_handle())
}

/// Fused ingest factory: resolves the typed producer once at route-collection
/// time; per message the returned closure runs deserialize + produce with no
/// erasure crossing.
fn ingest_factory<T>(
    record_key: String,
    deser: TypedMatchDeserializerFn<T>,
) -> crate::connector::IngestFactoryFn
where
    T: Send + Sync + 'static + Debug + Clone,
{
    Arc::new(move |db: &AimDb| {
        let producer = inbound_producer::<T>(db, &record_key);
        let deser = deser.clone();
        Arc::new(
            move |ctx: &crate::RuntimeContext, m: &crate::TopicMatch<'_>, payload: &[u8]| {
                producer.produce(deser(ctx, m, payload)?);
                Ok(())
            },
        ) as crate::connector::IngestFn
    })
}

/// Type alias for typed context-aware deserializer callbacks
///
/// Stays typed until `finish()` fuses it with the producer — no per-message
/// erasure.
type TypedContextDeserializerFn<T> =
    Arc<dyn Fn(crate::RuntimeContext, &[u8]) -> Result<T, String> + Send + Sync + 'static>;

/// Like [`TypedContextDeserializerFn`], also receiving the topic match.
type TypedMatchDeserializerFn<T> = Arc<
    dyn Fn(&crate::RuntimeContext, &crate::TopicMatch<'_>, &[u8]) -> Result<T, String>
        + Send
        + Sync
        + 'static,
>;

/// Builder for configuring inbound connector links (External → AimDB)
///
/// `'r` is the borrow of the registrar taken by `link_from()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub struct InboundConnectorBuilder<'r, 'a, T: Send + Sync + 'static + Debug + Clone> {
    registrar: &'r mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    context_deserializer: Option<TypedContextDeserializerFn<T>>,
    match_deserializer: Option<TypedMatchDeserializerFn<T>>,
    key: Option<(String, u16)>,
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

    /// Like [`with_deserializer`](Self::with_deserializer), also receiving the
    /// topic the message arrived on, its captures and its key. The context is
    /// borrowed, so no reference count changes per message.
    pub fn with_match_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&crate::RuntimeContext, &crate::TopicMatch<'_>, &[u8]) -> Result<T, String>
            + Send
            + Sync
            + 'static,
    {
        self.match_deserializer = Some(Arc::new(f));
        self
    }

    /// Assigns each value of capture `name` a [`KeyId`](crate::KeyId), up to
    /// `capacity` values per record. Messages with a value beyond that are
    /// dropped. A value keeps its key even if its payload fails to
    /// deserialize, and keys are never freed, so anyone who can publish under
    /// the pattern can fill the table.
    pub fn key(mut self, name: &str, capacity: u16) -> Self {
        self.key = Some((name.to_string(), capacity));
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
    pub fn finish(mut self) -> &'r mut RecordRegistrar<'a, T> {
        match self.link() {
            Ok(link) => self.registrar.rec.add_inbound_connector(link),
            Err(message) => {
                let key = self.registrar.record_key.clone();
                let error = crate::error::ConfigError::new(key, Some(self.url), message);
                self.registrar.rec.push_config_error(error);
            }
        }
        self.registrar
    }

    /// The link `finish()` registers, or why it is rejected.
    ///
    /// The buffer requirement and mutual exclusion with local producers
    /// (.source()/.transform()) are validated by `build()`: `.buffer()` may
    /// legitimately be called after `.link_from()`.
    fn link(&mut self) -> Result<crate::connector::InboundConnectorLink, String> {
        use crate::connector::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse(&self.url).map_err(|_| "Invalid connector URL")?;
        let record_key = self.registrar.record_key.clone();

        let deser: TypedMatchDeserializerFn<T> = match (
            self.context_deserializer.take(),
            self.match_deserializer.take(),
        ) {
            (Some(deser), None) => Arc::new(move |ctx, _m, bytes| deser(ctx.clone(), bytes)),
            (None, Some(deser)) => deser,
            (Some(_), Some(_)) => {
                return Err(
                    "Set either .with_deserializer() or .with_match_deserializer(), not both"
                        .into(),
                )
            }
            (None, None) => {
                return Err("Inbound connector requires a deserializer. Call \
                                .with_deserializer() or .with_match_deserializer()"
                    .into())
            }
        };

        // The `{…}` syntax; the connector checks the rest.
        let pattern = crate::TopicPattern::parse(url.resource_id()).map_err(|e| e.to_string())?;

        let key = match self.key.take() {
            None => None,
            Some((capture, capacity)) => {
                let Some(capacity) = core::num::NonZeroU16::new(capacity) else {
                    return Err(alloc::format!(
                        "key '{capture}' needs a capacity of at least 1"
                    ));
                };
                if !pattern.capture_names().any(|n| n == capture) {
                    return Err(alloc::format!(
                        "key '{capture}' is not a capture of the topic"
                    ));
                }
                let links = self.registrar.rec.inbound_connectors();
                if let Some((_, other)) = links.iter().find_map(|l| l.key.as_ref()) {
                    if *other != capacity {
                        return Err(alloc::format!(
                            "key '{capture}' has capacity {capacity}, another keyed link of \
                             this record has {other}"
                        ));
                    }
                }
                Some((capture, capacity))
            }
        };

        let scheme = url.scheme();
        if !self
            .registrar
            .connector_builders
            .iter()
            .any(|b| b.scheme() == scheme)
        {
            return Err(alloc::format!(
                "No connector registered for scheme '{scheme}'. Register via .with_connector()"
            ));
        }

        let mut link = InboundConnectorLink::new(url, ingest_factory(record_key, deser));
        link.config = core::mem::take(&mut self.config);
        link.key = key;
        link.topic_resolver = self.topic_resolver.take();
        Ok(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connector::{SerializeError, SerializedPayload, SerializedReader as _, TopicProvider},
        DbResult,
    };
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
    // Topic patterns and keys on inbound links
    // ====================================================================

    /// Runs `configure` against a fresh registrar with an `mqtt` connector
    /// and returns the record's links and recorded errors.
    fn register(
        configure: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>),
    ) -> (
        Vec<crate::connector::InboundConnectorLink>,
        Vec<crate::error::ConfigError>,
    ) {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));
        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();
        configure(&mut make_registrar(&mut rec, &builders, &extensions));
        (rec.inbound_connectors().to_vec(), drain_errors(&mut rec))
    }

    fn match_deser(
        _ctx: &crate::RuntimeContext,
        m: &crate::TopicMatch<'_>,
        _bytes: &[u8],
    ) -> Result<TestRecord, String> {
        Ok(TestRecord {
            value: m.topic().len() as i32,
        })
    }

    #[test]
    fn inbound_finish_registers_match_link_with_key() {
        let (links, errors) = register(|reg| {
            reg.link_from("mqtt://s/{device}/t")
                .key("device", 16)
                .with_match_deserializer(match_deser)
                .finish();
        });
        assert!(errors.is_empty(), "{errors:?}");
        let (capture, capacity) = links[0].key.clone().unwrap();
        assert_eq!((capture.as_str(), capacity.get()), ("device", 16));
    }

    #[test]
    fn inbound_finish_rejects_both_deserializers() {
        let (links, errors) = register(|reg| {
            reg.link_from("mqtt://s/{device}/t")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .with_match_deserializer(match_deser)
                .finish();
        });
        assert!(links.is_empty());
        assert!(errors[0].message.contains("not both"), "{:?}", errors[0]);
    }

    #[test]
    fn inbound_finish_rejects_invalid_pattern_syntax() {
        for topic in [
            "s/{d",
            "s/{}",
            "s/{d-1}",
            "{d}/{d}",
            "{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}",
        ] {
            let url = alloc::format!("mqtt://{topic}");
            let (links, errors) = register(|reg| {
                reg.link_from(&url)
                    .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                    .finish();
            });
            assert!(links.is_empty(), "{topic}");
            assert_eq!(errors.len(), 1, "{topic}");
            assert_eq!(errors[0].record_key, "test::Record");
            assert_eq!(errors[0].url.as_deref(), Some(url.as_str()));
        }
    }

    #[test]
    fn inbound_finish_rejects_bad_keys() {
        for (topic, key, capacity, needle) in [
            ("s/{d}/t", "device", 4, "not a capture"),
            ("s/{d}/t", "d", 0, "at least 1"),
        ] {
            let (links, errors) = register(|reg| {
                reg.link_from(&alloc::format!("mqtt://{topic}"))
                    .key(key, capacity)
                    .with_match_deserializer(match_deser)
                    .finish();
            });
            assert!(links.is_empty());
            assert!(errors[0].message.contains(needle), "{:?}", errors[0]);
        }
    }

    #[test]
    fn keyed_links_of_one_record_share_a_capacity() {
        let (links, errors) = register(|reg| {
            reg.link_from("mqtt://temp/{d}")
                .key("d", 8)
                .with_match_deserializer(match_deser)
                .finish();
            reg.link_from("mqtt://hum/{id}")
                .key("id", 8)
                .with_match_deserializer(match_deser)
                .finish();
            reg.link_from("mqtt://co2/{id}")
                .key("id", 16)
                .with_match_deserializer(match_deser)
                .finish();
        });
        assert_eq!(links.len(), 2);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("has 8"), "{:?}", errors[0]);
    }

    #[test]
    fn outbound_finish_rejects_patterns() {
        let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
        rec.set_buffer(Box::new(MockBuffer));
        let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
            vec![Box::new(MockConnectorBuilder {
                scheme: "mqtt".to_string(),
            })];
        let extensions = crate::extensions::Extensions::new();
        let mut reg = make_registrar(&mut rec, &builders, &extensions);

        reg.link_to("mqtt://out/{device}")
            .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
            .finish();

        assert!(rec.outbound_connectors().is_empty());
        let errors = drain_errors(&mut rec);
        assert!(errors[0].message.contains("cannot contain '{' or '}'"));
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

    /// A connector that claims every route for its scheme, as KNX and the
    /// session *clients* do.
    struct OwningConnectorBuilder(&'static str);

    impl crate::connector::ConnectorBuilder for OwningConnectorBuilder {
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
            self.0
        }
        fn owns_scheme(&self) -> bool {
            true
        }
    }

    /// Two scheme-owning connectors under one scheme is a configuration error,
    /// not a silently duplicated route set.
    #[tokio::test]
    async fn two_owning_connectors_on_one_scheme_fail_the_build() {
        let builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(OwningConnectorBuilder("knx"))
            .with_connector(OwningConnectorBuilder("knx"));

        let Err(err) = builder.build().await else {
            panic!("a second connector for an owned scheme must be rejected");
        };
        let msg = alloc::format!("{err}");
        assert!(
            msg.contains("More than one connector registered for scheme 'knx'"),
            "unexpected error: {msg}"
        );
    }

    /// Distinct schemes are fine — that is how a caller runs two of the same
    /// transport side by side.
    #[tokio::test]
    async fn owning_connectors_on_distinct_schemes_build() {
        let builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(OwningConnectorBuilder("knx"))
            .with_connector(OwningConnectorBuilder("mqtt"));

        assert!(builder.build().await.is_ok());
    }

    /// A connector that collects no routes — a session *server*, say — may be
    /// registered twice under one scheme: two endpoints onto one dispatch.
    /// `owns_scheme` defaults to `false`, so this must keep working.
    #[tokio::test]
    async fn two_non_owning_connectors_on_one_scheme_build() {
        let builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder)
            .with_connector(NoopConnectorBuilder);

        assert!(builder.build().await.is_ok());
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

    /// Routes `payload` on `topic` through the `mqtt` inbound router.
    fn route(db: &crate::AimDb, topic: &str, payload: &[u8]) {
        let router = db.inbound_router("mqtt", &Plus).expect("routes compile");
        router.route(topic, payload, &db.runtime_ctx()).unwrap();
    }

    /// End-to-end inbound path: bytes → fused ingest → typed buffer push,
    /// with no `Box<dyn Any>` in between.
    #[tokio::test]
    async fn ingest_roundtrip_produces_value() {
        let (db, last, count) = inbound_db(|reg| {
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
        })
        .await;

        route(&db, "cmd/in", &[1, 2, 3]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(last.load(Ordering::SeqCst), 3);

        // Bad bytes: the deserializer fails, nothing is produced.
        route(&db, "cmd/in", &[]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// The deserializer set last wins, whichever way its context is typed.
    #[tokio::test]
    async fn deserializer_set_last_wins() {
        let (db, last, _) = inbound_db(|reg| {
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                    Ok(TestRecord { value: 99 })
                })
                .finish();
        })
        .await;
        route(&db, "cmd/in", b"x");
        assert_eq!(last.load(Ordering::SeqCst), 99);
    }

    // ====================================================================
    // inbound_router: patterns, keys and connector-build errors
    // ====================================================================

    use crate::topic_pattern::test_support::Plus;

    /// A record `rec.in` whose buffer stores the last value and a count.
    async fn inbound_db(
        links: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>) + Send + 'static,
    ) -> (crate::AimDb, Arc<AtomicI32>, Arc<AtomicUsize>) {
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
            links(reg);
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");
        (db, last, count)
    }

    /// The key index, or -1 without a key.
    fn key_deser(
        _ctx: &crate::RuntimeContext,
        m: &crate::TopicMatch<'_>,
        _bytes: &[u8],
    ) -> Result<TestRecord, String> {
        Ok(TestRecord {
            value: m.key().map_or(-1, |k| k.index() as i32),
        })
    }

    #[tokio::test]
    async fn inbound_router_routes_patterns_with_shared_keys() {
        let keys: Arc<spin::Mutex<Vec<crate::KeyId>>> = Default::default();
        let seen = keys.clone();
        let (db, last, count) = inbound_db(move |reg| {
            reg.link_from("mqtt://temp/{d}")
                .key("d", 2)
                .with_match_deserializer(move |ctx, m, bytes| {
                    seen.lock().extend(m.key());
                    key_deser(ctx, m, bytes)
                })
                .finish();
            reg.link_from("mqtt://hum/{id}")
                .key("id", 2)
                .with_match_deserializer(key_deser)
                .finish();
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 100 }))
                .finish();
        })
        .await;

        let router = db.inbound_router("mqtt", &Plus).expect("routes compile");
        let subscriptions: Vec<String> = router
            .subscriptions()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(subscriptions, ["cmd/in", "hum/+", "temp/+"]);

        let ctx = db.runtime_ctx();
        let route = |topic: &str| {
            router.route(topic, b"", &ctx).unwrap();
            last.load(Ordering::SeqCst)
        };
        assert_eq!(route("temp/a"), 0);
        assert_eq!(route("hum/b"), 1);
        assert_eq!(route("hum/a"), 0, "one key table per record");
        assert_eq!(route("cmd/in"), 100);
        let produced = count.load(Ordering::SeqCst);
        route("temp/c");
        assert_eq!(count.load(Ordering::SeqCst), produced, "full table drops");

        let a = keys.lock()[0];
        assert_eq!(db.inbound_key_name("rec.in", a).as_deref(), Some("a"));
        assert_eq!(db.inbound_key_name("other", a), None);

        #[cfg(feature = "remote")]
        {
            let records = db.list_records();
            let info = records[0].inbound_keys.as_ref().expect("keyed record");
            assert_eq!(info.captures, ["d", "id"]);
            assert_eq!((info.capacity, info.assigned, info.dropped), (2, 2, 1));
        }
    }

    fn config_errors<T>(result: crate::DbResult<T>) -> Vec<crate::ConfigError> {
        match result {
            Err(crate::DbError::InvalidConfiguration { errors }) => errors,
            Err(e) => panic!("unexpected error {e:?}"),
            Ok(_) => panic!("expected configuration errors"),
        }
    }

    #[tokio::test]
    async fn inbound_router_rejects_links_it_cannot_compile() {
        let (db, _, _) = inbound_db(|reg| {
            reg.link_from("mqtt://s/{d}/t")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
            reg.link_from("mqtt://r/one")
                .with_topic_resolver(|| Some("r/{d".into()))
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
            reg.link_from("mqtt://k/{d}")
                .key("d", 4)
                .with_topic_resolver(|| Some("k/{x}".into()))
                .with_match_deserializer(key_deser)
                .finish();
        })
        .await;

        let errors = config_errors(db.inbound_router("mqtt", &Plus));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().all(|e| e.record_key == "rec.in"));
        assert!(errors[0].message.contains("unbalanced '{' in 'r/{d'"));
        assert!(errors[1]
            .message
            .contains("key 'd' is not a capture of 'k/{x}'"));

        let errors = config_errors(db.inbound_router("mqtt", &crate::ExactGrammar));
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0]
            .message
            .contains("does not support topic patterns"));
    }

    // ====================================================================
    // InboundDispatch: the same cases through the connector entry point
    // ====================================================================

    #[tokio::test]
    async fn inbound_dispatch_routes_patterns_with_shared_keys() {
        let (db, last, count) = inbound_db(|reg| {
            reg.link_from("mqtt://temp/{d}")
                .key("d", 2)
                .with_match_deserializer(key_deser)
                .finish();
            reg.link_from("mqtt://hum/{id}")
                .key("id", 2)
                .with_match_deserializer(key_deser)
                .finish();
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 100 }))
                .finish();
        })
        .await;

        let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
        let subscriptions: Vec<String> = inbound
            .subscriptions()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(subscriptions, ["cmd/in", "hum/+", "temp/+"]);
        assert_eq!(inbound.route_count(), 3);

        let dispatch = |topic: &str| {
            inbound.dispatch(topic, b"");
            last.load(Ordering::SeqCst)
        };
        assert_eq!(dispatch("temp/a"), 0);
        assert_eq!(dispatch("hum/b"), 1);
        assert_eq!(dispatch("hum/a"), 0, "one key table per record");
        assert_eq!(dispatch("cmd/in"), 100);
        let produced = count.load(Ordering::SeqCst);
        dispatch("temp/c");
        assert_eq!(count.load(Ordering::SeqCst), produced, "full table drops");
    }

    #[tokio::test]
    async fn inbound_dispatch_rejects_links_it_cannot_compile() {
        let (db, _, _) = inbound_db(|reg| {
            reg.link_from("mqtt://r/one")
                .with_topic_resolver(|| Some("r/{d".into()))
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
            reg.link_from("mqtt://k/{d}")
                .key("d", 4)
                .with_topic_resolver(|| Some("k/{x}".into()))
                .with_match_deserializer(key_deser)
                .finish();
        })
        .await;

        let errors = config_errors(crate::InboundDispatch::new(&db, "mqtt", &Plus));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().all(|e| e.record_key == "rec.in"));
        assert!(errors[0].message.contains("unbalanced '{' in 'r/{d'"));
        assert!(errors[1]
            .message
            .contains("key 'd' is not a capture of 'k/{x}'"));
    }

    #[tokio::test]
    async fn inbound_dispatch_clones_share_records() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::InboundDispatch>();

        let (db, last, count) = inbound_db(|reg| {
            reg.link_from("mqtt://cmd/in")
                .with_deserializer(|_ctx, bytes: &[u8]| {
                    Ok(TestRecord {
                        value: bytes.len() as i32,
                    })
                })
                .finish();
        })
        .await;

        let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).unwrap();
        let clone = inbound.clone();
        drop(inbound);
        clone.dispatch("cmd/in", b"abcd");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(last.load(Ordering::SeqCst), 4);
    }

    #[cfg(feature = "connector-session")]
    #[tokio::test]
    async fn pump_source_routes_through_the_given_router() {
        struct Once(Option<(String, crate::Payload)>);
        impl crate::Source for Once {
            fn next(&mut self) -> crate::BoxFut<'_, Option<(String, crate::Payload)>> {
                let next = self.0.take();
                Box::pin(async move { next })
            }
        }

        let (db, last, _) = inbound_db(|reg| {
            reg.link_from("mqtt://temp/{d}")
                .key("d", 2)
                .with_match_deserializer(key_deser)
                .finish();
        })
        .await;
        let router = db.inbound_router("mqtt", &Plus).unwrap();
        let source = Once(Some(("temp/a".into(), Arc::from(&b"x"[..]))));
        for pump in crate::pump_source(&db, router, source) {
            pump.await;
        }
        assert_eq!(last.load(Ordering::SeqCst), 0);
    }

    // ====================================================================
    // Fused outbound reader tests
    // ====================================================================

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
        topic: TopicSelector<TestRecord>,
    ) -> FusedReader<TestRecord> {
        let topic_capacity = match &topic {
            TopicSelector::Writer { capacity, .. } => *capacity,
            _ => 0,
        };
        FusedReader {
            inner: crate::buffer::Reader::new(Box::new(ScriptedReader { script })),
            serialize,
            serialize_into: None,
            topic,
            topic_buf: alloc::vec![0; topic_capacity].into_boxed_slice(),
        }
    }

    fn fused_reader_into(
        script: Vec<Result<TestRecord, crate::DbError>>,
        serialize: FusedSerializeFn<TestRecord>,
        serialize_into: FusedSerializeIntoFn<TestRecord>,
    ) -> FusedReader<TestRecord> {
        FusedReader {
            inner: crate::buffer::Reader::new(Box::new(ScriptedReader { script })),
            serialize,
            serialize_into: Some(serialize_into),
            topic: TopicSelector::None,
            topic_buf: Box::default(),
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
            TopicSelector::None,
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
                    Err(SerializeError::InvalidData)
                } else {
                    Ok(r.value.to_le_bytes().to_vec())
                }
            }),
            TopicSelector::None,
        );

        // One recv: the failing value is skipped, the next good one returned.
        let msg = reader.recv(&test_ctx()).await.expect("value");
        assert_eq!(msg.payload, 42i32.to_le_bytes().to_vec());
    }

    #[tokio::test]
    async fn fused_reader_into_uses_scratch_without_owned_fallback() {
        let owned_calls = Arc::new(AtomicUsize::new(0));
        let into_calls = Arc::new(AtomicUsize::new(0));
        let owned_counter = owned_calls.clone();
        let into_counter = into_calls.clone();
        let mut reader = fused_reader_into(
            vec![Ok(TestRecord { value: 7 })],
            Arc::new(move |_ctx, r| {
                owned_counter.fetch_add(1, Ordering::SeqCst);
                Ok(r.value.to_le_bytes().to_vec())
            }),
            Arc::new(move |_ctx, r, out| {
                into_counter.fetch_add(1, Ordering::SeqCst);
                let bytes = r.value.to_le_bytes();
                out.get_mut(..bytes.len())
                    .ok_or(SerializeError::BufferTooSmall)?
                    .copy_from_slice(&bytes);
                Ok(bytes.len())
            }),
        );
        let mut scratch = [0_u8; 8];

        let msg = reader
            .recv_into(&test_ctx(), &mut scratch)
            .await
            .expect("value");

        assert_eq!(msg.payload, SerializedPayload::Scratch { len: 4 });
        assert_eq!(&scratch[..4], 7i32.to_le_bytes().as_slice());
        assert_eq!(into_calls.load(Ordering::SeqCst), 1);
        assert_eq!(owned_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fused_reader_into_falls_back_once_when_scratch_is_small() {
        let owned_calls = Arc::new(AtomicUsize::new(0));
        let owned_counter = owned_calls.clone();
        let mut reader = fused_reader_into(
            vec![Ok(TestRecord { value: 9 })],
            Arc::new(move |_ctx, r| {
                owned_counter.fetch_add(1, Ordering::SeqCst);
                Ok(r.value.to_le_bytes().to_vec())
            }),
            Arc::new(|_ctx, _r, _out| Err(SerializeError::BufferTooSmall)),
        );
        let mut scratch = [0_u8; 2];

        let msg = reader
            .recv_into(&test_ctx(), &mut scratch)
            .await
            .expect("fallback value");

        assert_eq!(
            msg.payload,
            SerializedPayload::Owned(9i32.to_le_bytes().to_vec())
        );
        assert_eq!(owned_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fused_reader_into_rejects_invalid_length_and_skips_value() {
        let mut reader = fused_reader_into(
            vec![Ok(TestRecord { value: 1 }), Ok(TestRecord { value: 2 })],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            Arc::new(|_ctx, r, out| {
                if r.value == 1 {
                    return Ok(out.len() + 1);
                }
                let bytes = r.value.to_le_bytes();
                out[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }),
        );
        let mut scratch = [0_u8; 8];

        let msg = reader
            .recv_into(&test_ctx(), &mut scratch)
            .await
            .expect("second value");

        assert_eq!(msg.payload, SerializedPayload::Scratch { len: 4 });
        assert_eq!(&scratch[..4], 2i32.to_le_bytes().as_slice());
    }

    #[tokio::test]
    async fn fused_reader_into_skips_invalid_data() {
        let mut reader = fused_reader_into(
            vec![Ok(TestRecord { value: 1 }), Ok(TestRecord { value: 2 })],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            Arc::new(|_ctx, r, out| {
                if r.value == 1 {
                    return Err(SerializeError::InvalidData);
                }
                let bytes = r.value.to_le_bytes();
                out[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }),
        );
        let mut scratch = [0_u8; 8];

        let msg = reader
            .recv_into(&test_ctx(), &mut scratch)
            .await
            .expect("second value");

        assert_eq!(msg.payload, SerializedPayload::Scratch { len: 4 });
        assert_eq!(&scratch[..4], 2i32.to_le_bytes().as_slice());
    }

    /// The destination is resolved from the typed value while it is in hand.
    #[tokio::test]
    async fn fused_reader_resolves_dynamic_topic() {
        struct PositiveTopic;
        impl TopicProvider<TestRecord> for PositiveTopic {
            fn topic(&self, value: &TestRecord) -> Option<String> {
                (value.value > 0).then(|| alloc::format!("dyn/{}", value.value))
            }
        }

        let mut reader = fused_reader(
            vec![Ok(TestRecord { value: 5 }), Ok(TestRecord { value: 0 })],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            TopicSelector::Provider(Arc::new(PositiveTopic)),
        );
        let ctx = test_ctx();

        let first = reader.recv(&ctx).await.expect("value");
        assert_eq!(first.dest.as_deref(), Some("dyn/5"));

        let second = reader.recv(&ctx).await.expect("value");
        assert_eq!(second.dest, None); // falls back to the route default
    }

    fn writer(
        capacity: usize,
        f: impl Fn(&TestRecord, &mut crate::TopicBuf<'_>) -> Result<bool, crate::TopicOverflow>
            + Send
            + Sync
            + 'static,
    ) -> TopicSelector<TestRecord> {
        TopicSelector::Writer {
            capacity,
            writer: Arc::new(f),
        }
    }

    /// The same case with a written topic; `Ok(false)` uses the default.
    #[tokio::test]
    async fn fused_reader_resolves_written_topic() {
        use core::fmt::Write as _;
        let mut reader = fused_reader(
            vec![Ok(TestRecord { value: 5 }), Ok(TestRecord { value: 0 })],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            writer(8, |v, out| {
                if v.value <= 0 {
                    return Ok(false);
                }
                write!(out, "dyn/{}", v.value)?;
                Ok(true)
            }),
        );
        let ctx = test_ctx();

        let first = reader.recv(&ctx).await.expect("value");
        assert_eq!(first.dest.as_deref(), Some("dyn/5"));

        let second = reader.recv(&ctx).await.expect("value");
        assert_eq!(second.dest, None);
    }

    /// A topic that overflows skips its value, even when the writer ignores
    /// the error and returns `Ok(true)`.
    #[tokio::test]
    async fn fused_reader_skips_overflowing_topics() {
        use core::fmt::Write as _;
        let mut reader = fused_reader(
            vec![
                Ok(TestRecord { value: 123_456 }),
                Ok(TestRecord { value: 1_234_567 }),
                Ok(TestRecord { value: 7 }),
            ],
            Arc::new(|_ctx, r| Ok(r.value.to_le_bytes().to_vec())),
            writer(6, |v, out| {
                if v.value == 1_234_567 {
                    let _ = write!(out, "t/{}", v.value);
                    return Ok(true);
                }
                write!(out, "t/{}", v.value)?;
                Ok(true)
            }),
        );
        let ctx = test_ctx();

        // "t/123456" (8 bytes) and "t/1234567" (9 bytes) do not fit in 6.
        let msg = reader.recv(&ctx).await.expect("value");
        assert_eq!(msg.dest.as_deref(), Some("t/7"));
        assert_eq!(msg.payload, 7i32.to_le_bytes().to_vec());
    }

    /// `with_topic_fn` infers an unannotated closure's argument types, which
    /// a generic `W: TopicWriter<T>` bound cannot.
    #[tokio::test]
    async fn with_topic_fn_infers_and_writes_the_topic() {
        use core::fmt::Write as _;
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

        let mut builder = crate::AimDbBuilder::new()
            .runtime(Arc::new(MockRuntime))
            .with_connector(NoopConnectorBuilder);
        builder.configure::<TestRecord>("rec.out", |reg| {
            reg.buffer_raw(Box::new(CannedBuffer));
            reg.link_to("mqtt://tele/out")
                .with_topic_fn(16, |v, out| {
                    write!(out, "dyn/{}", v.value)?;
                    Ok(true)
                })
                .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_outbound_routes("mqtt");
        let mut reader = routes[0].source.subscribe();
        let msg = reader.recv(&db.runtime_ctx()).await.expect("value");
        assert_eq!(msg.dest.as_deref(), Some("dyn/5"));
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
        impl TopicProvider<TestRecord> for FixedTopic {
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
                .with_serializer_into(4, |_ctx, r: &TestRecord, out| {
                    let encoded = r.value.to_le_bytes();
                    let dest = out
                        .get_mut(..encoded.len())
                        .ok_or(SerializeError::BufferTooSmall)?;
                    dest.copy_from_slice(&encoded);
                    Ok(encoded.len())
                })
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build must succeed");

        let routes = db.collect_outbound_routes("mqtt");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].topic, "tele/out");
        assert_eq!(routes[0].source.serializer_scratch_capacity(), Some(4));

        let mut reader = routes[0].source.subscribe();
        let ctx = db.runtime_ctx();
        let mut scratch = [0u8; 4];
        let msg = reader.recv_into(&ctx, &mut scratch).await.expect("value");
        assert_eq!(msg.dest.as_deref(), Some("dyn/5"));
        assert_eq!(msg.payload, SerializedPayload::Scratch { len: 4 });
        assert_eq!(scratch, 5i32.to_le_bytes());

        let closed = reader
            .recv_into(&ctx, &mut scratch)
            .await
            .expect_err("buffer closed");
        assert!(matches!(closed, crate::DbError::BufferClosed { .. }));
    }
}
