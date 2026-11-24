use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use core::fmt::Debug;
use core::future::Future;

use crate::typed_api::{
    link_inbound::{ingest_factory, TypedMatchDeserializerFn},
    link_outbound::{
        serialize_outbound, write_topic, ConsumerFactoryFn, FusedSerializeFn, FusedSerializeIntoFn,
        TypedRoute,
    },
    Consumer, InboundConnectorBuilder, InboundLink, Input, OutboundConnectorBuilder, OutboundLink,
    Output,
};
use crate::typed_record::TypedRecord;
use crate::AimDb;

// ============================================================================
// RecordRegistrar - Fluent registration API
// ============================================================================

/// Type alias for typed context-aware serializer callbacks
///
/// Stays typed until registration fuses it with the consumer — no per-message
/// erasure.
pub(super) type TypedContextSerializerFn<T> = Arc<
    dyn Fn(crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
        + Send
        + Sync
        + 'static,
>;

/// Typed into-slice serializer stored by [`OutboundLinkBuilder`].
pub(super) type TypedContextSerializerIntoFn<T> = Arc<
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
///
/// `S` is the record's [`SchemaState`](crate::SchemaState): `configure`
/// hands out [`Declared`](crate::Declared) for a key that declares a data
/// contract (`Key<T>`, `#[record_key(contract = T)]`) and
/// [`Undeclared`](crate::Undeclared) otherwise. Contract-only verbs
/// (`linked_to`, `publish`, `observe`, …) are implemented only for
/// `Declared`, so using one on a `Name<T>` record does not compile.
/// Everything else works in both states; extension traits implement for any
/// `S: SchemaState`.
pub struct RecordRegistrar<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    S: crate::SchemaState = crate::Undeclared,
> {
    /// The typed record being configured
    pub(crate) rec: &'a mut TypedRecord<T>,
    /// Connector builders indexed by scheme
    pub(crate) connector_builders: &'a [Box<dyn crate::connector::ConnectorBuilder>],
    /// The builder's id, checked against
    /// [`ConnectorHandle`](crate::ConnectorHandle)s on links.
    pub(crate) builder_id: u32,
    /// The record key for this record
    pub(crate) record_key: String,
    pub(crate) key_link_address: Option<String>,
    /// Extension storage from the builder — allows external crates (e.g.
    /// `aimdb-persistence`) to retrieve typed state inside `.persist()`.
    pub(crate) extensions: &'a crate::extensions::Extensions,
    /// The most recently registered stage, so `.with_name()` knows what to name.
    /// Tracked even when the `observability` feature is off (then it's just unused).
    #[cfg_attr(not(feature = "observability"), allow(dead_code))]
    pub(crate) last_stage: Option<(StageKind, usize)>,
    pub(crate) _schema: core::marker::PhantomData<fn() -> S>,
}

impl<'a, T, S: crate::SchemaState> RecordRegistrar<'a, T, S>
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

    /// Returns the key this record is registered under.
    ///
    /// Extension crates use it to name per-record state; `aimdb-persistence`
    /// stores a record's history under this key.
    pub fn record_key(&self) -> &str {
        &self.record_key
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

    /// Sets the data-contract schema this record carries, reported in the
    /// dependency graph (`graph.nodes`) and in `record.list`.
    ///
    /// Only `configure` calls this, from the key's [`TypedKey::SCHEMA`](crate::TypedKey::SCHEMA):
    /// the key is the one source of a record's schema (063). Setting the same
    /// schema again does nothing; a different one fails `build()`.
    #[cfg(test)]
    pub(crate) fn schema(&mut self, name: &str, version: u32) -> &mut Self {
        self.rec.set_schema(name, version);
        self
    }

    /// Registers a producer service for this record type.
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext)
    /// (time + logging capabilities) and a pre-resolved [`Producer<T>`](crate::Producer); it is
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
    pub fn buffer_raw(
        &mut self,
        buffer: Box<dyn crate::buffer::DynBuffer<crate::Stamped<T>>>,
    ) -> &mut Self {
        self.rec.set_buffer(buffer);
        self
    }

    /// Configures a buffer with metadata tracking
    pub fn buffer_with_cfg(
        &mut self,
        buffer: Box<dyn crate::buffer::DynBuffer<crate::Stamped<T>>>,
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
    pub fn transform<K, F>(&mut self, input_key: K, build_fn: F) -> &mut Self
    where
        K: crate::record_id::TypedKey,
        F: FnOnce(
            crate::transform::TransformBuilder<K::Contract, T>,
        ) -> crate::transform::TransformPipeline<K::Contract, T>,
    {
        let input_key_str = input_key.as_str().to_string();
        let builder = crate::transform::TransformBuilder::<K::Contract, T>::new(input_key_str);
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
    /// Builds a link using the address declared on this record's typed key.
    /// A missing address is reported by `build()`.
    pub fn link_to_key(&mut self) -> OutboundConnectorBuilder<'_, 'a, T, S> {
        let address = self.key_link_address.clone().unwrap_or_default();
        self.link_to(&address)
    }
    /// Builds a link using the address declared on this record's typed key.
    /// A missing address is reported by `build()`.
    pub fn link_from_key(&mut self) -> InboundConnectorBuilder<'_, 'a, T, S> {
        let address = self.key_link_address.clone().unwrap_or_default();
        self.link_from(&address)
    }

    /// Link TO external system (outbound: AimDB → External)
    ///
    /// Subscribes to buffer updates and publishes them to an external system.
    pub fn link_to(&mut self, url: &str) -> OutboundConnectorBuilder<'_, 'a, T, S> {
        OutboundLink::new(url).attach(self)
    }

    /// Link FROM external system (inbound: External → AimDB)
    ///
    /// Subscribes to an external data source and produces values into this record's buffer.
    pub fn link_from(&mut self, url: &str) -> InboundConnectorBuilder<'_, 'a, T, S> {
        InboundLink::new(url).attach(self)
    }

    /// Wires `input` as a writer of this record.
    ///
    /// Takes anything implementing [`Input<T>`], such as an [`InboundLink`].
    /// Conflicting writers are reported by `build()`, exactly as for
    /// `.source()` and `.link_from()`; several inbound links on one record
    /// are allowed (fan-in).
    pub fn input(&mut self, input: impl Input<T, S>) -> &mut Self {
        input.wire(self);
        self
    }

    /// Wires `output` as a reader of this record.
    ///
    /// Takes anything implementing [`Output<T>`], such as an
    /// [`OutboundLink`]. A following [`with_name`](Self::with_name) names the
    /// stage the output registered, as after `.link_to(..).finish()`.
    pub fn output(&mut self, output: impl Output<T, S>) -> &mut Self {
        output.wire(self);
        self
    }
}

// ============================================================================
// Link registration - the one validation path for attached and detached links
// ============================================================================

/// The wire state a detached link of either direction carries into
/// registration.
pub(super) trait WireLink: Sized {
    fn wire_url(&self) -> &str;
    fn wire_parts(
        &mut self,
    ) -> (
        &mut crate::link_wire::WireOptions,
        &mut Vec<(String, String)>,
    );
    #[allow(clippy::type_complexity)]
    fn take_wire_finalizer(
        &mut self,
    ) -> Option<Arc<dyn Fn(Self) -> Result<Self, String> + Send + Sync>>;
}

macro_rules! wire_link {
    ($link:ident) => {
        impl<T: Send + Sync + 'static + Debug + Clone> WireLink for $link<T> {
            fn wire_url(&self) -> &str {
                &self.url
            }
            fn wire_parts(
                &mut self,
            ) -> (
                &mut crate::link_wire::WireOptions,
                &mut Vec<(String, String)>,
            ) {
                (&mut self.wire, &mut self.config)
            }
            fn take_wire_finalizer(
                &mut self,
            ) -> Option<Arc<dyn Fn(Self) -> Result<Self, String> + Send + Sync>> {
                self.wire_finalizer.take()
            }
        }
    };
}
pub(super) use wire_link;

impl<'a, T, S: crate::SchemaState> RecordRegistrar<'a, T, S>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Resolves the carrier and runs the link's wire finalizer: the one path
    /// for both directions. Records a config error and returns `None` if the
    /// link is refused.
    fn finalize_wire<L: WireLink>(&mut self, mut link: L) -> Option<L> {
        let url = link.wire_url().to_string();
        let finalized = (|| {
            let (wire, config) = link.wire_parts();
            wire.take_carrier(config)?;
            if let Some(finalize) = link.take_wire_finalizer() {
                link = finalize(link)?;
            } else if link.wire_parts().0.carrier == crate::EnvelopeCarrier::Inline {
                return Err(
                    "inline envelopes require with_envelope or with_versioned_codec".into(),
                );
            }
            let (wire, config) = link.wire_parts();
            wire.record_carrier(config);
            Ok::<L, String>(link)
        })();
        match finalized {
            Ok(link) => Some(link),
            Err(message) => {
                self.rec.push_config_error(crate::ConfigError::new(
                    self.record_key.clone(),
                    Some(url),
                    message,
                ));
                None
            }
        }
    }

    /// Validates and registers an outbound link, or records why not.
    pub(super) fn push_outbound(&mut self, link: OutboundLink<T>) {
        let Some(mut link) = self.finalize_wire(link) else {
            return;
        };

        use crate::connector::{ConnectorLink, LinkAddress};
        use crate::error::ConfigError;

        let record_key = self.record_key.clone();

        if let Some(handle) = link.connector {
            match crate::connector::resolve_link_url(
                self.builder_id,
                self.connector_builders,
                handle,
                &link.url,
            ) {
                Ok(url) => link.url = url,
                Err(message) => {
                    self.rec.push_config_error(ConfigError::new(
                        record_key,
                        Some(link.url),
                        message,
                    ));
                    return;
                }
            }
        }

        let Ok(url) = LinkAddress::parse(&link.url) else {
            self.rec.push_config_error(ConfigError::new(
                record_key,
                Some(link.url),
                "Invalid connector URL",
            ));
            return;
        };

        let url_string = url.to_string();
        let scheme = url.scheme().to_string();

        if url.resource_id().contains(['{', '}']) {
            self.rec.push_config_error(ConfigError::new(
                record_key,
                Some(link.url),
                "Outbound topics cannot contain '{' or '}'",
            ));
            return;
        }

        // Adapt the stored serializer to the fused calling convention. Stays
        // typed: fused with the consumer below, no `Box<dyn Any>` per message
        //.
        let serialize: FusedSerializeFn<T> = if let Some(ser) = link.context_serializer {
            Arc::new(move |ctx: &crate::RuntimeContext, value: &T| ser(ctx.clone(), value))
        } else {
            self.rec.push_config_error(ConfigError::new(
                record_key,
                Some(link.url),
                "Outbound connector requires a serializer. Call .with_serializer()",
            ));
            return;
        };

        let serialize_into: Option<(usize, FusedSerializeIntoFn<T>)> =
            link.context_serializer_into.map(|(capacity, ser)| {
                let adapted: FusedSerializeIntoFn<T> = Arc::new(
                    move |ctx: &crate::RuntimeContext, value: &T, out: &mut [u8]| {
                        ser(ctx.clone(), value, out)
                    },
                );
                (capacity, adapted)
            });

        // Validation: Check that connector builder is registered
        let has_connector = self.connector_builders.iter().any(|b| b.scheme() == scheme);

        if !has_connector {
            self.rec.push_config_error(ConfigError::new(
                record_key,
                Some(url_string),
                alloc::format!(
                    "No connector registered for scheme '{scheme}'. Register via .with_connector()"
                ),
            ));
            return;
        }

        // Register the link as a profiling stage (so `.with_name()` can name it
        // and the consumer it creates can be timed).
        #[cfg(feature = "observability")]
        let link_metrics = {
            let (idx, metrics) = self.rec.profiling_mut().push_link();
            self.last_stage = Some((StageKind::Link, idx));
            metrics
        };
        #[cfg(not(feature = "observability"))]
        {
            self.last_stage = Some((StageKind::Link, 0));
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
            let record_key = self.record_key.clone();
            Arc::new(move |db: &AimDb| {
                let typed_rec = db
                    .inner()
                    .get_typed_record_by_key_raw::<T>(&record_key)
                    .unwrap_or_else(|e| {
                        panic!(
                            "outbound link: record '{record_key}' lookup failed ({e:?}) — \
                             this is a bug in aimdb-core"
                        )
                    });
                let buffer = typed_rec.buffer_handle().unwrap_or_else(|| {
                    panic!(
                        "outbound link: record '{record_key}' has no buffer despite \
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

        let topic_writer = link.topic_writer;
        let has_topic_writer = topic_writer.is_some();

        // Renders the record's current value through this link's topic writer
        // and serializer, for late-join snapshots (`OutboundLatest`).
        let latest_fn: crate::outbound::LatestFn = {
            let record_key = self.record_key.clone();
            let topic_writer = topic_writer.clone();
            let serialize = serialize.clone();
            let serialize_into = serialize_into.clone();
            Arc::new(
                move |db: &AimDb,
                      ctx: &crate::RuntimeContext,
                      accept: &dyn Fn(Option<&str>) -> bool| {
                    let sample = db
                        .inner()
                        .get_typed_record_by_key_raw::<T>(&record_key)
                        .ok()?
                        .latest()?
                        .into_stamped();

                    let topic = match &topic_writer {
                        None => None,
                        Some((capacity, writer)) => {
                            let mut buf = alloc::vec![0u8; *capacity];
                            let mut out = crate::connector::TopicBuf::new(&mut buf);
                            let written = write_topic(&**writer, &sample.value, &mut out).ok()?;
                            let len = out.len();
                            // `TopicBuf` only ever holds whole `&str`s.
                            written
                                .then(|| String::from_utf8(buf[..len].to_vec()).ok())
                                .flatten()
                        }
                    };
                    // Serialize only for a topic the caller wants.
                    if !accept(topic.as_deref()) {
                        return None;
                    }

                    let mut scratch = alloc::vec![0u8; serialize_into.as_ref().map_or(0, |(capacity, _)| *capacity)];
                    let payload = match serialize_outbound(
                        &serialize,
                        serialize_into.as_ref().map(|(_, f)| f),
                        ctx,
                        &sample.value,
                        &mut scratch,
                    )
                    .ok()?
                    {
                        crate::outbound::StagedPayload::Scratch(len) => scratch[..len].to_vec(),
                        crate::outbound::StagedPayload::Owned(bytes) => bytes,
                    };

                    Some(crate::outbound::Rendered {
                        topic,
                        payload,
                        metadata: *sample.metadata(),
                    })
                },
            )
        };

        // Subscribed when `OutboundRoutes` is built.
        let route_factory: crate::outbound::RouteFactoryFn = {
            Arc::new(move |db: &AimDb| {
                let (topic_capacity, writer) = match &topic_writer {
                    Some((capacity, writer)) => (*capacity, Some(writer.clone())),
                    None => (0, None),
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
                }
            })
        };

        // Named `out`, not `link`: `out.config = link.config` must read the
        // user's link, not the new one.
        let mut out = ConnectorLink::new(url, route_factory, latest_fn, has_topic_writer);
        out.config = link.config;

        // Routes are built later, when a connector builds its `OutboundRoutes`.
        self.rec.add_outbound_connector(out);
    }

    /// Validates and registers an inbound link, or records why not.
    pub(super) fn push_inbound(&mut self, link: InboundLink<T>) {
        let Some(mut link) = self.finalize_wire(link) else {
            return;
        };

        match self.inbound_link(&mut link) {
            Ok(inbound) => self.rec.add_inbound_connector(inbound),
            Err(message) => {
                let key = self.record_key.clone();
                let error = crate::error::ConfigError::new(key, Some(link.url), message);
                self.rec.push_config_error(error);
            }
        }
    }

    /// The link `push_inbound()` registers, or why it is rejected.
    ///
    /// The buffer requirement and mutual exclusion with local producers
    /// (.source()/.transform()) are validated by `build()`: `.buffer()` may
    /// legitimately be called after `.link_from()`.
    fn inbound_link(
        &self,
        link: &mut InboundLink<T>,
    ) -> Result<crate::connector::InboundConnectorLink, String> {
        use crate::connector::{InboundConnectorLink, LinkAddress};

        if let Some(handle) = link.connector {
            link.url = crate::connector::resolve_link_url(
                self.builder_id,
                self.connector_builders,
                handle,
                &link.url,
            )?;
        }
        let url = LinkAddress::parse(&link.url).map_err(|_| "Invalid connector URL")?;
        let record_key = self.record_key.clone();

        let deser: TypedMatchDeserializerFn<T> = match (
            link.context_deserializer.take(),
            link.match_deserializer.take(),
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

        let key = match link.key.take() {
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
                let links = self.rec.inbound_connectors();
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
        if !self.connector_builders.iter().any(|b| b.scheme() == scheme) {
            return Err(alloc::format!(
                "No connector registered for scheme '{scheme}'. Register via .with_connector()"
            ));
        }

        // Named `inbound`, not `link`, for the same reason as in `push_outbound`.
        let mut inbound = InboundConnectorLink::new(url, ingest_factory(record_key, deser))
            .with_upgrade_versions(&link.upgrade_versions);
        inbound.config = core::mem::take(&mut link.config);
        inbound.key = key;
        inbound.topic_resolver = link.topic_resolver.take();
        Ok(inbound)
    }
}
