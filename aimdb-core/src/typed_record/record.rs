use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{any::Any, fmt::Debug};

use crate::{
    buffer::DynBuffer,
    typed_record::{lock, BoxFuture, ConsumerServiceFn, Mutex, ProducerServiceFn},
    AnyRecord,
};

#[cfg(feature = "remote")]
use crate::JsonRecordAccess;

#[cfg(feature = "remote")]
use crate::typed_record::events::{report, RemoteWriteEvent};

#[cfg(feature = "observability")]
use crate::profiling::RecordProfilingMetrics;

use crate::transform::TransformProfiling;

#[cfg(feature = "remote")]
use crate::typed_record::json_adapters::JsonReaderAdapter;

/// Type alias for a record's type-erased JSON codec (feature `remote`)
#[cfg(feature = "remote")]
pub(super) type RecordCodec<T> = Arc<dyn crate::codec::JsonCodec<T>>;

/// Wrapper for a record's latest value with optional serialization.
///
/// Created by `TypedRecord::latest()`. Carries the value together with the
/// metadata of the write that stored it. Core methods (`get()`,
/// `into_inner()`, `Deref`, `metadata()`, `stamp()`) work in both std and
/// no_std. JSON serialization (`.as_json()`) requires the `remote` feature.
pub struct RecordValue<T> {
    inner: crate::Stamped<T>,
    #[cfg(feature = "remote")]
    codec: Option<RecordCodec<T>>,
}

impl<T> RecordValue<T> {
    /// Create a new RecordValue with optional codec
    #[cfg(feature = "remote")]
    fn new(inner: crate::Stamped<T>, codec: Option<RecordCodec<T>>) -> Self {
        Self { inner, codec }
    }

    /// Create a new RecordValue without codec (codec feature off)
    #[cfg(not(feature = "remote"))]
    fn new(inner: crate::Stamped<T>, _codec: Option<()>) -> Self {
        Self { inner }
    }

    /// Get a reference to the underlying value
    pub fn get(&self) -> &T {
        &self.inner.value
    }

    /// Consume the wrapper and return the underlying value
    pub fn into_inner(self) -> T {
        self.inner.value
    }

    /// The metadata of the write that stored this value.
    pub fn metadata(&self) -> &crate::WriteMetadata {
        self.inner.metadata()
    }

    /// When this value was written, on the instance clock.
    pub fn stamp(&self) -> crate::Stamp {
        self.inner.stamp()
    }

    /// Consume the wrapper and return the value with its write metadata.
    pub fn into_stamped(self) -> crate::Stamped<T> {
        self.inner
    }

    /// Serialize the value to JSON (feature `remote`)
    ///
    /// Returns `Some(JsonValue)` if the record was configured with
    /// `.with_remote_access()`, otherwise `None`. Available on no_std + alloc
    /// when the `remote` feature is enabled. Without it, use `.get()`,
    /// `.into_inner()`, or `Deref` for direct access.
    #[cfg(feature = "remote")]
    pub fn as_json(&self) -> Option<serde_json::Value> {
        self.codec.as_ref()?.encode(&self.inner.value)
    }
}

impl<T> core::ops::Deref for RecordValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.value
    }
}

/// Helper for collecting all build-time futures for a specific record type.
///
/// Captures record type `T` via PhantomData so `AnyRecord` stays object-safe.
/// Returns every future the record needs (producer, transform, consumers) so
/// `AimDbBuilder::build()` can hand them to the `AimDbRunner`.
pub struct RecordFutureCollector<T> {
    _phantom: core::marker::PhantomData<T>,
}

impl<T> RecordFutureCollector<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Collects all futures (producer, transform, consumers) for a record.
    ///
    /// Downcasts the type-erased `AnyRecord` to `TypedRecord<T>` then returns
    /// every future the record contributes to the runner. Source/transform are
    /// mutually exclusive — at most one will appear.
    pub fn collect_all_futures(
        record: &dyn AnyRecord,
        db: &Arc<crate::builder::AimDb>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>> {
        use crate::DbError;

        // Downcast to TypedRecord<T>
        let typed_record: &TypedRecord<T> = record
            .as_any()
            .downcast_ref::<TypedRecord<T>>()
            .ok_or_else(|| DbError::RecordNotFound {
                record_name: core::any::type_name::<T>().to_string(),
            })?;

        let mut futures = Vec::new();

        if typed_record.has_producer() {
            if let Some(f) = typed_record.collect_producer_future(db, record_key)? {
                futures.push(f);
            }
        }

        if typed_record.has_transform() {
            futures.extend(typed_record.collect_transform_futures(db, record_key)?);
        }

        if typed_record.consumer_count() > 0 {
            futures.extend(typed_record.collect_consumer_futures(db, record_key)?);
        }

        Ok(futures)
    }
}

/// Typed record storage with producer/consumer functions
///
/// Stores type-safe producer and consumer functions with optional buffering for async dispatch.
pub struct TypedRecord<T: Send + 'static + Debug + Clone> {
    /// Optional producer service - a task that generates data
    /// This will be auto-spawned during build() if present
    /// Stored as `FnOnce` that takes (`Producer<T>`, `RuntimeContext`) and returns a `Future`
    /// Wrapped in Mutex for interior mutability (needed to take() during spawning)
    producer: Mutex<Option<ProducerServiceFn<T>>>,

    /// List of consumer/tap tasks - wrapped in Mutex for Sync + taking out during spawn
    /// Each is spawned as an independent background task that subscribes to the buffer
    /// Using Mutex provides the Sync bound required by AnyRecord trait
    consumers: Mutex<Vec<ConsumerServiceFn<T>>>,

    /// Transform descriptor — mutually exclusive with producer.
    /// If set, this record is a reactive derivation from one or more input records.
    /// Uses the same Mutex pattern for take()-during-spawn.
    transform: Mutex<Option<crate::transform::TransformDescriptor<T>>>,

    /// Optional buffer for async dispatch
    /// When present, produce() enqueues to buffer instead of direct call
    ///
    /// Stored as `Arc` (not `Box`) so `Producer<T>` and `Consumer<T>` can hold
    /// a pre-resolved handle to the same buffer.
    pub(super) buffer: Option<Arc<dyn DynBuffer<crate::Stamped<T>>>>,

    /// The instance clock, bound by `build()` before any writer exists.
    clock: Option<Arc<dyn crate::clock::InstanceClock>>,

    /// Buffer configuration cached for metadata / dependency-graph reporting.
    /// Set via `set_buffer_cfg()` after `set_buffer()`.
    buffer_cfg: Option<crate::buffer::BufferCfg>,

    /// List of outbound connector links (AimDB → External)
    /// Each link represents a protocol connector (MQTT, Kafka, HTTP, etc.)
    outbound_connectors: Vec<crate::connector::ConnectorLink>,

    /// List of inbound connector links (External → AimDB)
    /// Each link spawns a background task that subscribes to an external source
    /// and produces values into this record's buffer
    inbound_connectors: Vec<crate::connector::InboundConnectorLink>,

    /// The data-contract schema the record carries, if one was set
    /// (`RecordRegistrar::schema`). Reported in the graph and `record.list`.
    schema: Option<crate::graph::SchemaInfo>,

    /// Per-stage profiling metrics (feature `observability`).
    /// Stages are appended here in the same order they are registered on the
    /// `RecordRegistrar`, which matches the order the spawn machinery iterates them.
    #[cfg(feature = "observability")]
    profiling: RecordProfilingMetrics,

    /// Whether this record allows writes via remote access (feature
    /// `remote`). Interior-mutable so the security policy can mark it after
    /// `build()`; read by `collect_metadata` into `RecordMetadata.writable`.
    #[cfg(feature = "remote")]
    writable: portable_atomic::AtomicBool,

    /// Type-erased JSON tree/byte codec (feature `remote`).
    /// `Some` iff the record opted into JSON via `.with_remote_access()`.
    /// `RecordValue::as_json` and — under `remote` — the AimX read, write, and
    /// subscribe paths route through it. Built from a `SerdeJsonCodec` where
    /// the `T: RemoteSerialize` bound is known at the call site.
    #[cfg(feature = "remote")]
    pub(super) remote_codec: Option<RecordCodec<T>>,

    /// Configuration mistakes recorded during registration instead of
    /// panicking. Drained by `AimDbBuilder::build()`, which fills
    /// in the record key and reports all of them via
    /// [`DbError::InvalidConfiguration`](crate::DbError::InvalidConfiguration).
    config_errors: Vec<crate::error::ConfigError>,
}

impl<T: Send + 'static + Debug + Clone> TypedRecord<T> {
    /// Creates a new empty typed record
    ///
    /// Call `.with_remote_access()` to enable JSON (feature `remote`).
    pub fn new() -> Self {
        Self {
            producer: Mutex::new(None),
            consumers: Mutex::new(Vec::new()),
            transform: Mutex::new(None),
            buffer: None,
            clock: None,
            buffer_cfg: None,
            outbound_connectors: Vec::new(),
            inbound_connectors: Vec::new(),
            schema: None,
            #[cfg(feature = "observability")]
            profiling: RecordProfilingMetrics::new(),
            #[cfg(feature = "remote")]
            writable: portable_atomic::AtomicBool::new(false),
            #[cfg(feature = "remote")]
            remote_codec: None,
            config_errors: Vec::new(),
        }
    }

    /// Records a configuration mistake to be reported from `build()`.
    ///
    /// Errors recorded here never panic; `build()` drains them (filling in
    /// the record key) and fails with `DbError::InvalidConfiguration`.
    pub(crate) fn push_config_error(&mut self, err: crate::error::ConfigError) {
        self.config_errors.push(err);
    }

    /// Sets the schema the record carries. Setting the same schema again does
    /// nothing; a different one is a configuration error reported by `build()`.
    pub fn set_schema(&mut self, name: &str, version: u32) {
        match &self.schema {
            None => {
                self.schema = Some(crate::graph::SchemaInfo {
                    name: name.to_string(),
                    version,
                })
            }
            Some(s) if s.name == name && s.version == version => {}
            Some(s) => {
                let message = alloc::format!(
                    "schema {name} v{version} set, but the record already carries {} v{}",
                    s.name,
                    s.version
                );
                self.push_config_error(crate::error::ConfigError::new("", None, message));
            }
        }
    }

    /// The schema the record carries, if one was set
    pub fn schema(&self) -> Option<&crate::graph::SchemaInfo> {
        self.schema.as_ref()
    }

    /// Stage profiling metrics for this record (feature `observability`).
    #[cfg(feature = "observability")]
    pub fn profiling(&self) -> &RecordProfilingMetrics {
        &self.profiling
    }

    /// Mutable access to the stage profiling metrics — used during registration
    /// to append per-stage entries and assign names.
    #[cfg(feature = "observability")]
    pub(crate) fn profiling_mut(&mut self) -> &mut RecordProfilingMetrics {
        &mut self.profiling
    }

    /// Sets the producer service for this record
    ///
    /// Long-running task that generates data via `producer.produce()`. Auto-spawned during `build()`.
    ///
    /// A producer is mutually exclusive with a `.transform()` and with any
    /// `.link_from()` inbound connector (all three would race on the buffer),
    /// and only one producer is allowed. Violations are recorded — not
    /// panicked — and reported from `build()`; the conflicting registration
    /// is skipped.
    pub fn set_producer<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::RuntimeContext, crate::Producer<T>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Same-stage duplicate: a second .source() would silently overwrite
        // the first. Cross-stage exclusivity (.source()/.transform()/
        // .link_from()) is validated once, in build().
        if lock(&self.producer).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "This record type already has a producer service",
            ));
            return;
        }

        // Box the future-returning function
        let boxed_fn = Box::new(
            move |ctx: crate::RuntimeContext,
                  producer: crate::Producer<T>|
                  -> BoxFuture<'static, ()> { Box::pin(f(ctx, producer)) },
        );

        // Store it in the mutex
        *lock(&self.producer) = Some(boxed_fn);
    }

    /// Adds a consumer function for this record
    ///
    /// Consumer functions are spawned as independent background tasks that
    /// subscribe to the buffer and process values asynchronously.
    /// Multiple consumers can be registered for the same record type.
    ///
    /// # Arguments
    /// * `f` - A function that takes the `RuntimeContext` and a `Consumer<T>`, and returns a Future
    pub fn add_consumer<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::RuntimeContext, crate::Consumer<T>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Box the future to make it trait object compatible
        let boxed = Box::new(
            move |ctx: crate::RuntimeContext,
                  consumer: crate::Consumer<T>|
                  -> BoxFuture<'static, ()> { Box::pin(f(ctx, consumer)) },
        );

        lock(&self.consumers).push(boxed);
    }

    /// Sets the transform descriptor for this record.
    ///
    /// A transform is mutually exclusive with `.source()` and with any
    /// `.link_from()` inbound connector — all three write to the same buffer
    /// and would race as last-writer-wins. That exclusivity is validated in
    /// `build()`; a duplicate transform is recorded here — not panicked — and
    /// reported from `build()` with the conflicting registration skipped.
    pub(crate) fn set_transform(&mut self, descriptor: crate::transform::TransformDescriptor<T>) {
        // Same-stage duplicate: only one transform per record. Cross-stage
        // exclusivity (.source()/.transform()/.link_from()) is validated
        // once, in build().
        if lock(&self.transform).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "Record already has a .transform(); only one is allowed.",
            ));
            return;
        }
        *lock(&self.transform) = Some(descriptor);
    }

    /// Returns whether a transform is registered for this record.
    pub fn has_transform(&self) -> bool {
        lock(&self.transform).is_some()
    }

    /// Returns how this record gets its values.
    ///
    /// Used for constructing the dependency graph. Determines the record's origin:
    /// - `Source` if a producer service is registered
    /// - `Link` if inbound connectors are registered
    /// - `Transform` or `TransformJoin` if a transform is registered
    /// - `Passive` otherwise
    pub fn record_origin(&self) -> crate::graph::RecordOrigin {
        // Check for transform first (most specific)
        let transform_keys = lock(&self.transform).as_ref().map(|t| t.input_keys.clone());

        if let Some(input_keys) = transform_keys {
            // Asked of the iterator, not of `len()`: a pattern, not a trusted
            // invariant.
            let mut inputs = input_keys.into_iter();
            return match (inputs.next(), inputs.next()) {
                (Some(input), None) => crate::graph::RecordOrigin::Transform { input },
                (first, second) => crate::graph::RecordOrigin::TransformJoin {
                    inputs: first.into_iter().chain(second).chain(inputs).collect(),
                },
            };
        }

        // Check for inbound connector (link)
        if let Some(first_link) = self.inbound_connectors.first() {
            return crate::graph::RecordOrigin::Link {
                protocol: first_link.url.scheme().to_string(),
                address: first_link.url.to_string(),
            };
        }

        // Check for producer service (source)
        if lock(&self.producer).is_some() {
            return crate::graph::RecordOrigin::Source;
        }

        // No producer — passive record
        crate::graph::RecordOrigin::Passive
    }

    /// Returns the input keys for the registered transform (if any).
    ///
    /// Used during build-time validation to check that all input records exist.
    pub fn transform_input_keys(&self) -> Option<Vec<String>> {
        lock(&self.transform).as_ref().map(|t| t.input_keys.clone())
    }

    /// Returns each transform input's key and the type it is read as.
    ///
    /// Used during build-time validation to check that every input record
    /// holds the type the transform reads. Empty if no transform is registered.
    pub fn transform_input_types(&self) -> Vec<(String, core::any::TypeId, &'static str)> {
        lock(&self.transform)
            .as_ref()
            .map(|t| {
                t.input_keys
                    .iter()
                    .zip(&t.input_types)
                    .map(|(key, &(type_id, type_name))| (key.clone(), type_id, type_name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Attaches transform profiling for [`Self::collect_transform_futures`].
    ///
    /// A join gets its metrics attached to `producer` directly; a single-input
    /// transform gets them back as the forwarder's profiling arg.
    #[cfg(feature = "observability")]
    fn with_transform_profiling(
        &self,
        mut producer: crate::Producer<T>,
        is_single_input: bool,
        db: &Arc<crate::AimDb>,
    ) -> (crate::Producer<T>, TransformProfiling) {
        let profiling_entry = self.profiling.transforms().first();
        if !is_single_input {
            // Join: producer cadence is the throughput proxy — attach it directly.
            if let Some(entry) = profiling_entry {
                producer.set_profiling(entry.metrics.clone(), db.profiling_clock().clone());
            }
        }

        let profiling_arg = if is_single_input {
            profiling_entry.map(|entry| (entry.metrics.clone(), db.profiling_clock().clone()))
        } else {
            None
        };
        (producer, profiling_arg)
    }

    /// No-op without `observability`: returns `producer` unchanged.
    #[cfg(not(feature = "observability"))]
    fn with_transform_profiling(
        &self,
        producer: crate::Producer<T>,
        _is_single_input: bool,
        _db: &Arc<crate::AimDb>,
    ) -> (crate::Producer<T>, TransformProfiling) {
        (producer, ())
    }

    /// Collects the transform task future and any fan-in forwarder futures.
    ///
    /// Single-input transforms return one future; join transforms return the
    /// trigger-loop future plus one forwarder future per input. Returns an
    /// empty `Vec` if no transform is registered.
    pub fn collect_transform_futures(
        &self,
        db: &Arc<crate::AimDb>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>>
    where
        T: Sync,
    {
        // Take the transform descriptor (can only collect once)
        let descriptor = lock(&self.transform).take();

        if let Some(desc) = descriptor {
            log_info!(
                "🔄 Collecting transform futures for '{}' (inputs: {:?})",
                record_key,
                desc.input_keys
            );

            // Create Producer<T> bound to a pre-resolved write handle for this record.
            let producer = crate::Producer::new(self.writer_handle()?);
            let is_single_input = desc.input_keys.len() == 1;
            let (producer, profiling_arg) =
                self.with_transform_profiling(producer, is_single_input, db);
            let collected = (desc.build_fn)(producer, db.clone(), record_key, profiling_arg);

            let mut out = Vec::with_capacity(1 + collected.fanin_futures.len());
            out.push(collected.task_future);
            out.extend(collected.fanin_futures);
            Ok(out)
        } else {
            Ok(Vec::new())
        }
    }

    /// Sets the buffer for this record
    ///
    /// When set, `produce()` enqueues values for async dispatch to consumers.
    pub fn set_buffer(&mut self, buffer: Box<dyn DynBuffer<crate::Stamped<T>>>) {
        // The buffer trait object hides the original BufferCfg, so callers must
        // supply it via set_buffer_cfg() if they want accurate buffer_info().
        self.buffer_cfg = None;
        // `Arc::from(Box<dyn _>)` reuses the existing heap allocation; the
        // public API stays Box-flavoured to avoid churn at adapter / test call
        // sites, while internally we share via Arc so producers/consumers can
        // hold a pre-resolved handle.
        self.buffer = Some(Arc::from(buffer));
    }

    /// Sets the buffer configuration (for metadata tracking)
    pub fn set_buffer_cfg(&mut self, cfg: crate::buffer::BufferCfg) {
        self.buffer_cfg = Some(cfg);
    }

    /// Returns buffer type name and capacity (for dependency graph)
    pub fn buffer_info(&self) -> (String, Option<usize>) {
        if let Some(cfg) = &self.buffer_cfg {
            let cap = match cfg {
                crate::buffer::BufferCfg::SpmcRing { capacity } => Some(*capacity),
                _ => None,
            };
            (cfg.name().to_string(), cap)
        } else if self.buffer.is_some() {
            // Buffer set via buffer_raw() without a recorded cfg.
            ("unknown".to_string(), None)
        } else {
            ("none".to_string(), None)
        }
    }

    /// Returns whether a buffer is configured
    ///
    /// # Returns
    /// `true` if buffer is set, `false` otherwise
    pub fn has_buffer(&self) -> bool {
        self.buffer.is_some()
    }

    /// Returns a reference to the buffer if present
    ///
    /// # Returns
    /// `Some(&dyn DynBuffer<crate::Stamped<T>>)` if buffer is set, `None` otherwise
    pub fn buffer(&self) -> Option<&dyn DynBuffer<crate::Stamped<T>>> {
        self.buffer.as_deref()
    }

    /// Returns a fresh `Arc<dyn WriteHandle<T>>` bound to this record's buffer
    /// and the instance clock. Used at build time by the spawn machinery to
    /// pre-resolve `Producer<T>` handles.
    ///
    /// Fails for a record that is not part of a built database: `build()`
    /// binds the clock, and a write without one would go unstamped.
    pub(crate) fn writer_handle(&self) -> crate::DbResult<Arc<dyn crate::buffer::WriteHandle<T>>>
    where
        T: Send + Clone + 'static,
    {
        let clock = self
            .clock
            .clone()
            .ok_or_else(|| crate::DbError::InvalidOperation {
                operation: "write".to_string(),
                reason: "the record has no instance clock; only records of a built database accept writes"
                    .to_string(),
            })?;
        Ok(Arc::new(crate::buffer::RecordWriter::new(
            self.buffer.clone(),
            clock,
        )))
    }

    /// Returns a clone of the buffer `Arc` (or `None` if no buffer is
    /// configured). Used at build time to pre-resolve `Consumer<T>` handles.
    pub(crate) fn buffer_handle(&self) -> Option<Arc<dyn DynBuffer<crate::Stamped<T>>>> {
        self.buffer.clone()
    }

    /// Subscribes to the buffer for this record type
    ///
    /// Returns an ergonomic, allocation-free [`Reader<T>`](crate::buffer::Reader)
    /// handle.
    ///
    /// # Errors
    /// Returns `DbError::MissingConfiguration` if no buffer configured
    pub fn subscribe(&self) -> crate::DbResult<crate::buffer::Reader<T>> {
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| crate::DbError::missing_configuration("buffer"))?;

        Ok(crate::buffer::Reader::new(buffer.subscribe_boxed()))
    }

    /// Adds an outbound connector link for external system integration
    ///
    /// Bridges records to external protocols (MQTT, Kafka, HTTP, etc.).
    /// Multiple connectors supported per record.
    pub fn add_outbound_connector(&mut self, link: crate::connector::ConnectorLink) {
        self.outbound_connectors.push(link);
    }

    /// Installs the JSON value codec for this record (feature `remote`)
    ///
    /// Enables `record.latest()?.as_json()` everywhere, and — under the
    /// `remote` feature — the AimX protocol (`record.get` / `set` /
    /// `subscribe`). Requires `remote` and `T: RemoteSerialize`
    /// (blanket-impl'd for every `Serialize + DeserializeOwned` type). Works on
    /// no_std + alloc.
    #[cfg(feature = "remote")]
    pub fn with_remote_access(&mut self) -> &mut Self
    where
        T: crate::codec::RemoteSerialize + 'static,
    {
        // Capture the serde-backed codec where the bound is statically known.
        self.remote_codec = Some(Arc::new(crate::codec::SerdeJsonCodec));

        log_info!(
            "with_remote_access() called for record type: {}",
            core::any::type_name::<T>()
        );

        self
    }

    /// Validate a type-erased remote write and return its configured codec.
    ///
    /// Both the JSON-tree and JSON-byte entrypoints use this helper so the
    /// no-producer/no-transform rule and error ordering cannot drift apart.
    #[cfg(feature = "remote")]
    fn remote_write_codec(&self, operation: &str) -> crate::DbResult<RecordCodec<T>> {
        use crate::DbError;

        if self.has_producer() || self.has_transform() {
            report(RemoteWriteEvent::Rejected {
                operation,
                record_type: core::any::type_name::<T>(),
            });

            return Err(DbError::permission_denied(alloc::format!(
                "Cannot set record '{}' - has active producer or transform. \
                 Use internal application logic instead. \
                 Remote access can only set configuration records without producers.",
                core::any::type_name::<T>()
            )));
        }

        let codec = self.remote_codec.clone().ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Record '{}' not configured with .with_remote_access(). \
                 Cannot deserialize from JSON.",
                core::any::type_name::<T>()
            ))
        })?;

        if self.buffer.is_none() {
            return Err(DbError::runtime_error(alloc::format!(
                "Record '{}' has no buffer configured. \
                 Cannot produce value without buffer.",
                core::any::type_name::<T>()
            )));
        }

        Ok(codec)
    }

    #[cfg(feature = "remote")]
    fn commit_remote_write(&self, value: T) -> crate::DbResult<()> {
        log_debug!(
            "Successfully deserialized JSON to type: {}",
            core::any::type_name::<T>()
        );

        // Push through the unified write path. This also marks metadata as
        // updated, matching the original `set_from_json` path.
        self.writer_handle()?.push(value);

        log_info!(
            "Successfully set value from JSON for record: {}",
            core::any::type_name::<T>()
        );
        Ok(())
    }

    /// Returns a reference to the registered outbound connectors
    ///
    /// # Returns
    /// A slice of outbound connector links
    pub fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.outbound_connectors
    }

    /// Returns all inbound connector links (External → AimDB)
    ///
    /// # Returns
    /// A slice of inbound connector links
    pub fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink] {
        &self.inbound_connectors
    }

    /// Adds an inbound connector link (External → AimDB)
    ///
    /// Called by `.link_from()` builder API during record configuration.
    ///
    /// An inbound connector is mutually exclusive with `.source()` and
    /// `.transform()` — all three write to the same buffer and would race as
    /// last-writer-wins. Violations are recorded — not panicked — and
    /// reported from `build()`; the conflicting registration is skipped.
    /// Multiple inbound connectors on the same record are permitted (fan-in).
    pub fn add_inbound_connector(&mut self, link: crate::connector::InboundConnectorLink) {
        // Cross-stage exclusivity with .source()/.transform() is validated
        // once, in build().
        self.inbound_connectors.push(link);
    }

    /// Returns the number of registered consumers
    ///
    /// # Returns
    /// The count of consumers
    pub fn consumer_count(&self) -> usize {
        lock(&self.consumers).len()
    }

    /// Attaches the `i`-th tap's profiling to `consumer`, for
    /// [`Self::collect_consumer_futures`].
    #[cfg(feature = "observability")]
    fn set_consumer_profiling(&self, consumer: &mut crate::Consumer<T>, i: usize, db: &crate::AimDb)
    where
        T: Sync,
    {
        if let Some(entry) = self.profiling.tap(i) {
            consumer.set_profiling(entry.metrics.clone(), db.profiling_clock().clone());
        }
    }

    /// No-op without `observability`.
    #[cfg(not(feature = "observability"))]
    fn set_consumer_profiling(
        &self,
        _consumer: &mut crate::Consumer<T>,
        _i: usize,
        _db: &crate::AimDb,
    ) where
        T: Sync,
    {
    }

    /// Collects all registered consumer (`.tap()`) futures.
    ///
    /// Consumes registered consumers (FnOnce) — can only be called once. The returned
    /// futures are pushed into the build-time accumulator by `RecordFutureCollector`
    /// and driven by `AimDbRunner::run()`.
    pub fn collect_consumer_futures(
        &self,
        db: &Arc<crate::AimDb>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>>
    where
        T: Sync,
    {
        // `record_key` shows up in the no-buffer error message (std only).
        #[cfg(not(feature = "std"))]
        let _ = record_key;

        log_debug!(
            "Collecting {} consumer futures for record type {}",
            self.consumer_count(),
            core::any::type_name::<T>()
        );

        // Take all consumers from the Mutex
        let consumers = core::mem::take(&mut *lock(&self.consumers));

        // Invariant: taps are pushed to `profiling` in the same order consumers are
        // added (both happen in `RecordRegistrar::tap`), so index `i` lines up.
        #[cfg(feature = "observability")]
        debug_assert_eq!(self.profiling.tap_count(), consumers.len());

        let mut futures = Vec::with_capacity(consumers.len());

        // Pre-resolve the buffer handle once — every consumer shares the same Arc.
        // A `.tap()` requires a buffer; surface the misconfiguration here with the
        // record key in the message rather than panicking inside `Consumer::new`.
        let buffer_arc = self.buffer_handle().ok_or_else(|| {
            crate::DbError::missing_configuration(alloc::format!(
                "buffer for record '{}' (required by .tap())",
                record_key
            ))
        })?;

        for (i, consumer_fn) in consumers.into_iter().enumerate() {
            // Create a Consumer<T> bound to a pre-resolved buffer handle.
            let mut consumer = crate::typed_api::Consumer::new(buffer_arc.clone());
            self.set_consumer_profiling(&mut consumer, i, db);

            futures.push(consumer_fn(db.runtime_ctx(), consumer));
        }

        Ok(futures)
    }

    /// Attaches the source's profiling metrics to `producer`, for
    /// [`Self::collect_producer_future`].
    #[cfg(feature = "observability")]
    fn set_source_profiling(&self, producer: &mut crate::Producer<T>, db: &Arc<crate::AimDb>)
    where
        T: Sync,
    {
        if let Some(entry) = self.profiling.source(0) {
            producer.set_profiling(entry.metrics.clone(), db.profiling_clock().clone());
        }
    }

    /// No-op without `observability`.
    #[cfg(not(feature = "observability"))]
    fn set_source_profiling(&self, _producer: &mut crate::Producer<T>, _db: &Arc<crate::AimDb>)
    where
        T: Sync,
    {
    }

    /// Collects the producer-service future, if one is registered.
    pub fn collect_producer_future(
        &self,
        db: &Arc<crate::AimDb>,
        record_key: &str,
    ) -> crate::DbResult<Option<BoxFuture<'static, ()>>>
    where
        T: Sync,
    {
        // Take the producer service (can only collect once)
        let service = lock(&self.producer).take();

        if let Some(service_fn) = service {
            log_debug!(
                "Collecting producer service future for record '{}' (type {})",
                record_key,
                core::any::type_name::<T>()
            );
            #[cfg(not(feature = "tracing"))]
            let _ = record_key;

            // Create Producer<T> bound to a pre-resolved write handle for this record.
            let mut producer = crate::typed_api::Producer::new(self.writer_handle()?);
            self.set_source_profiling(&mut producer, db);

            Ok(Some(service_fn(db.runtime_ctx(), producer)))
        } else {
            Ok(None)
        }
    }

    /// Returns whether a producer service is registered
    pub fn has_producer(&self) -> bool {
        lock(&self.producer).is_some()
    }

    /// Marks this record as writable for remote access
    #[cfg(feature = "remote")]
    pub fn set_writable(&self, writable: bool) {
        self.writable
            .store(writable, portable_atomic::Ordering::SeqCst);
    }

    /// Returns the latest produced value
    ///
    /// Returns the most recent value wrapped in `RecordValue<T>`, or `None` if no
    /// value has been produced yet, the record has no buffer, or the buffer has
    /// **no canonical latest** — i.e. [`SpmcRing`](crate::buffer::BufferCfg::SpmcRing)
    /// (a ring is a stream/backlog; read it via a subscriber/drain instead).
    /// Non-blocking.
    ///
    /// **Both std and no_std**: Direct access via `Deref`, `.get()`, `.into_inner()`
    ///
    /// **std only**: `.as_json()` (if `.with_remote_access()` configured)
    pub fn latest(&self) -> Option<RecordValue<T>> {
        // Read buffer-native storage via one peek(), so value and metadata
        // come from the same write. Records without a buffer return None.
        let sample = self.buffer.as_ref()?.peek()?;
        #[cfg(feature = "remote")]
        {
            Some(RecordValue::new(sample, self.remote_codec.clone()))
        }
        #[cfg(not(feature = "remote"))]
        {
            Some(RecordValue::new(sample, None))
        }
    }

    /// The build error for a remote-access record without a buffer, for
    /// [`AnyRecord::drain_config_errors`].
    ///
    /// A remote-access record has no fallback storage — reads/writes go
    /// straight to the buffer. With no buffer, `record.get`/`latest()` return
    /// not_found and `record.set` silently discards the value. Fail at build()
    /// so the buffer is added explicitly instead of surfacing as a silent
    /// runtime no-op.
    #[cfg(feature = "remote")]
    fn missing_remote_buffer_error(&self) -> Option<crate::error::ConfigError> {
        (self.remote_codec.is_some() && !self.has_buffer()).then(|| {
            crate::error::ConfigError::new(
                "",
                None,
                "record has .with_remote_access() but no buffer; \
                 add a buffer (e.g. .buffer(BufferCfg::SingleLatest)) so record.get/set work",
            )
        })
    }

    /// No-op without `remote`.
    #[cfg(not(feature = "remote"))]
    fn missing_remote_buffer_error(&self) -> Option<crate::error::ConfigError> {
        None
    }
}

impl<T: Send + 'static + Debug + Clone> Default for TypedRecord<T> {
    fn default() -> Self {
        Self::new()
    }
}

// BREAKING CHANGE (v0.2.0): TypedRecord now requires T: Sync
// This enables safe concurrent access to records from multiple connector tasks
// in the bidirectional routing system. The Sync bound propagates from AnyRecord
// trait and ensures thread-safe sharing of record values.
impl<T: Send + Sync + 'static + Debug + Clone> AnyRecord for TypedRecord<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn bind_clock(&mut self, clock: Arc<dyn crate::clock::InstanceClock>) {
        self.clock = Some(clock);
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn drain_config_errors(&mut self) -> Vec<crate::error::ConfigError> {
        let mut errors = core::mem::take(&mut self.config_errors);
        errors.extend(self.missing_remote_buffer_error());
        errors
    }

    fn set_writable_erased(&self, writable: bool) {
        #[cfg(feature = "remote")]
        {
            self.writable
                .store(writable, portable_atomic::Ordering::SeqCst);
        }
        #[cfg(not(feature = "remote"))]
        {
            let _ = writable; // Suppress unused warning
        }
    }

    #[cfg(feature = "remote")]
    fn json_access(&self) -> Option<&dyn JsonRecordAccess> {
        Some(self)
    }

    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.outbound_connectors
    }

    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink] {
        &self.inbound_connectors
    }

    fn schema(&self) -> Option<&crate::graph::SchemaInfo> {
        self.schema.as_ref()
    }

    fn consumer_count(&self) -> usize {
        TypedRecord::consumer_count(self)
    }

    fn has_producer(&self) -> bool {
        TypedRecord::has_producer(self)
    }

    fn has_buffer(&self) -> bool {
        TypedRecord::has_buffer(self)
    }

    fn has_transform(&self) -> bool {
        TypedRecord::has_transform(self)
    }

    fn record_origin(&self) -> crate::graph::RecordOrigin {
        TypedRecord::record_origin(self)
    }

    fn buffer_info(&self) -> (String, Option<usize>) {
        TypedRecord::buffer_info(self)
    }

    fn transform_input_keys(&self) -> Option<Vec<String>> {
        TypedRecord::transform_input_keys(self)
    }

    fn transform_input_types(&self) -> Vec<(String, core::any::TypeId, &'static str)> {
        TypedRecord::transform_input_types(self)
    }

    fn type_name(&self) -> &'static str {
        core::any::type_name::<T>()
    }

    #[cfg(feature = "remote")]
    fn collect_metadata(
        &self,
        type_id: core::any::TypeId,
        key: crate::record_id::StringKey,
        id: crate::record_id::RecordId,
    ) -> crate::remote::RecordMetadata {
        let (buffer_type, buffer_capacity) = if let Some(cfg) = &self.buffer_cfg {
            let cap = match cfg {
                crate::buffer::BufferCfg::SpmcRing { capacity } => Some(*capacity),
                _ => None,
            };
            (cfg.name().to_string(), cap)
        } else {
            ("none".to_string(), None)
        };

        // Computed on demand from the record's static structure; core keeps no
        // per-record metadata state (no created_at/last_update tracking).
        let metadata = crate::remote::RecordMetadata::new(
            id,
            key,
            type_id,
            core::any::type_name::<T>().to_string(),
            self.record_origin(),
            buffer_type,
            buffer_capacity,
            if self.has_producer() { 1 } else { 0 },
            self.consumer_count(),
            self.writable.load(portable_atomic::Ordering::SeqCst),
            self.outbound_connectors.len(),
        )
        .with_schema(self.schema.as_ref());

        #[cfg(feature = "observability")]
        let metadata = {
            let mut metadata = metadata;
            metadata.inbound_routes = self
                .inbound_connectors
                .iter()
                .map(|link| crate::InboundRouteMetrics {
                    link: link.url.to_string(),
                    stats: link.stats(),
                })
                .collect();
            metadata
        };

        // Add buffer metrics if available
        #[cfg(feature = "observability")]
        let metadata = {
            if let Some(ref buffer) = self.buffer {
                if let Some(snapshot) = buffer.metrics_snapshot() {
                    metadata.with_buffer_metrics(snapshot)
                } else {
                    metadata
                }
            } else {
                metadata
            }
        };

        // Attach stage profiling metrics when the feature is enabled.
        #[cfg(feature = "observability")]
        let metadata = metadata.with_stage_profiling(self.profiling.snapshot());

        // Attach signal-gauge statistics (from `.observe()`) when enabled.
        #[cfg(feature = "observability")]
        let metadata = metadata.with_signal_stats(self.profiling.signal_snapshot());

        metadata
    }

    #[cfg(feature = "observability")]
    fn reset_profiling(&self) {
        self.profiling.reset_all();
    }

    #[cfg(feature = "observability")]
    fn reset_buffer_metrics(&self) {
        if let Some(buf) = &self.buffer {
            buf.reset_metrics();
        }
    }
}

#[cfg(feature = "remote")]
impl<T: Send + Sync + 'static + Debug + Clone> JsonRecordAccess for TypedRecord<T> {
    fn latest_json(&self) -> Option<crate::Stamped<serde_json::Value>> {
        log_debug!(
            "latest_json called for type: {}",
            core::any::type_name::<T>()
        );

        // One peek(): payload and metadata come from the same write. Records
        // without a buffer return None.
        let codec = self.remote_codec.as_ref()?;
        let sample = self.buffer.as_ref()?.peek()?;
        let result = sample.as_ref().try_map(|v| codec.encode(v).ok_or(())).ok();

        log_debug!("Serialization result: {:?}", result.is_some());

        result
    }

    fn latest_json_bytes(&self) -> Option<crate::Stamped<Vec<u8>>> {
        log_debug!(
            "latest_json_bytes called for type: {}",
            core::any::type_name::<T>()
        );

        let codec = self.remote_codec.as_ref()?;
        let sample = self.buffer.as_ref()?.peek()?;
        let result = sample
            .as_ref()
            .try_map(|v| codec.encode_to_vec(v).ok_or(()))
            .ok();

        log_debug!("Byte serialization result: {:?}", result.is_some());

        result
    }

    fn subscribe_json(&self) -> crate::DbResult<Box<dyn crate::buffer::JsonBufferReader + Send>> {
        use crate::DbError;

        log_debug!(
            "subscribe_json called for type: {}",
            core::any::type_name::<T>()
        );

        // 1. Check if serialization is configured
        let codec = self.remote_codec.clone().ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Record '{}' not configured with .with_remote_access(). \
                 Cannot subscribe to JSON stream.",
                core::any::type_name::<T>()
            ))
        })?;

        // 2. Subscribe to the buffer (the adapter polls the erased reader
        //    directly, so it takes the boxed reader rather than the ergonomic
        //    `Reader<T>` wrapper).
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| DbError::missing_configuration("buffer"))?;

        // 3. Wrap in JsonReaderAdapter
        let json_reader = JsonReaderAdapter::new(buffer.subscribe_boxed(), codec);

        log_debug!(
            "Successfully created JSON subscription for type: {}",
            core::any::type_name::<T>()
        );

        Ok(Box::new(json_reader))
    }

    fn set_from_json(&self, json_value: serde_json::Value) -> crate::DbResult<()> {
        use crate::DbError;

        log_debug!(
            "set_from_json called for type: {}",
            core::any::type_name::<T>()
        );

        let codec = self.remote_write_codec("set_from_json")?;

        let value: T = codec.decode(&json_value).ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Failed to deserialize JSON to type '{}'. \
                 JSON structure does not match the expected schema.",
                core::any::type_name::<T>()
            ))
        })?;

        self.commit_remote_write(value)
    }

    fn set_from_json_bytes(&self, bytes: &[u8]) -> crate::DbResult<()> {
        use crate::DbError;

        log_debug!(
            "set_from_json_bytes called for type: {}",
            core::any::type_name::<T>()
        );

        let codec = self.remote_write_codec("set_from_json_bytes")?;
        let value: T = codec.decode_slice(bytes).ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Failed to deserialize JSON to type '{}'. \
                 JSON structure does not match the expected schema.",
                core::any::type_name::<T>()
            ))
        })?;

        self.commit_remote_write(value)
    }
}
