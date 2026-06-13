//! Type-safe record storage using TypeId
//!
//! Provides type-safe record identification using Rust's `TypeId` for compile-time safety.
//!
//! # Feature Support
//!
//! All layers below work on both `std` and `no_std + alloc`; they are gated by
//! capability feature, not by `std`:
//!
//! - **always**: Core API (`TypedRecord`, `latest()`, `RecordValue`, producer/consumer).
//! - **`json-serialize`**: the JSON value codec — `.with_remote_access()` installs it
//!   and `record.latest()?.as_json()` reads it.
//! - **`remote-access`**: record metadata (`collect_metadata` → `RecordMetadata`,
//!   the `writable` flag) plus the type-erased JSON read/write/subscribe methods
//!   (`latest_json` / `set_from_json` / `subscribe_json`) used by the AimX server.
//!
//! Without `json-serialize`, use `record.latest()` + `Deref` for value access and
//! implement custom serialization for embedded protocols (CBOR, MessagePack, etc.).

use core::any::Any;
use core::fmt::Debug;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

#[cfg(feature = "profiling")]
use crate::profiling::RecordProfilingMetrics;

#[cfg(feature = "std")]
type Mutex<T> = std::sync::Mutex<T>;
#[cfg(not(feature = "std"))]
type Mutex<T> = spin::Mutex<T>;

/// Locks one of the `TypedRecord` field mutexes, hiding the std/spin API
/// difference (`std::sync::Mutex::lock` returns a `LockResult`; `spin` returns
/// the guard directly). A poisoned std mutex is unrecoverable in this code, so
/// `.unwrap()` is the correct response. The returned guard derefs to `T` on
/// both sides, so call sites are identical.
#[cfg(feature = "std")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap()
}
#[cfg(not(feature = "std"))]
fn lock<T>(m: &Mutex<T>) -> spin::MutexGuard<'_, T> {
    m.lock()
}

use crate::buffer::DynBuffer;

/// Type alias for a record's type-erased JSON codec (feature `json-serialize`)
#[cfg(feature = "json-serialize")]
type RecordCodec<T> = Arc<dyn crate::codec::JsonCodec<T>>;

/// Wrapper for a record's latest value with optional serialization
///
/// Created by `TypedRecord::latest()`. Core methods (`get()`, `into_inner()`, `Deref`) work in
/// both std and no_std. JSON serialization (`.as_json()`) requires std feature.
pub struct RecordValue<T> {
    value: T,
    #[cfg(feature = "json-serialize")]
    codec: Option<RecordCodec<T>>,
}

impl<T> RecordValue<T> {
    /// Create a new RecordValue with optional codec
    #[cfg(feature = "json-serialize")]
    fn new(value: T, codec: Option<RecordCodec<T>>) -> Self {
        Self { value, codec }
    }

    /// Create a new RecordValue without codec (codec feature off)
    #[cfg(not(feature = "json-serialize"))]
    fn new(value: T, _codec: Option<()>) -> Self {
        Self { value }
    }

    /// Get a reference to the underlying value
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Consume the wrapper and return the underlying value
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Serialize the value to JSON (feature `json-serialize`)
    ///
    /// Returns `Some(JsonValue)` if the record was configured with
    /// `.with_remote_access()`, otherwise `None`. Available on no_std + alloc
    /// when the `json-serialize` feature is enabled. Without it, use `.get()`,
    /// `.into_inner()`, or `Deref` for direct access.
    #[cfg(feature = "json-serialize")]
    pub fn as_json(&self) -> Option<serde_json::Value> {
        self.codec.as_ref()?.encode(&self.value)
    }
}

impl<T: Clone> RecordValue<T> {
    /// Clone the underlying value
    pub fn cloned(&self) -> T {
        self.value.clone()
    }
}

impl<T> core::ops::Deref for RecordValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// Adapter that wraps a typed BufferReader and serializes values to JSON (std only)
///
/// Bridges the gap between typed buffers and type-erased JSON streaming for
/// remote access subscriptions. Each `recv_json()` call:
/// 1. Receives a typed value `T` from the buffer
/// 2. Serializes it to JSON using the configured serializer
/// 3. Returns the JSON value
///
/// Used internally by `TypedRecord::subscribe_json()`.
#[cfg(feature = "remote-access")]
struct JsonReaderAdapter<T: Clone + Send + 'static> {
    /// The underlying typed buffer reader
    inner: Box<dyn crate::buffer::BufferReader<T> + Send>,
    /// JSON codec (from .with_remote_access())
    codec: RecordCodec<T>,
}

#[cfg(feature = "remote-access")]
impl<T: Clone + Send + 'static> crate::buffer::JsonBufferReader for JsonReaderAdapter<T> {
    fn recv_json(
        &mut self,
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<Output = Result<serde_json::Value, crate::DbError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            // Receive typed value from buffer
            let value = self.inner.recv().await?;

            // Serialize to JSON
            self.codec
                .encode(&value)
                .ok_or_else(|| crate::DbError::runtime_error("Failed to serialize value to JSON"))
        })
    }

    fn try_recv_json(&mut self) -> Result<serde_json::Value, crate::DbError> {
        // Non-blocking receive from underlying typed buffer
        let value = self.inner.try_recv()?;

        // Serialize to JSON using the configured codec
        self.codec
            .encode(&value)
            .ok_or_else(|| crate::DbError::runtime_error("Failed to serialize value to JSON"))
    }
}

// Type alias for boxed futures
pub(crate) type BoxFuture<'a, T> =
    core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;

/// Type alias for consumer service closure stored in TypedRecord
/// Each consumer receives the [`RuntimeContext`](crate::RuntimeContext) and a
/// Consumer<T> handle for subscribing to the buffer
type ConsumerServiceFn<T> =
    Box<dyn FnOnce(crate::RuntimeContext, crate::Consumer<T>) -> BoxFuture<'static, ()> + Send>;

/// Type alias for producer service closure stored in TypedRecord
/// Takes (RuntimeContext, Producer<T>) and returns a Future
/// This will be auto-spawned during build().
/// `Send` only — `Sync` is not required because the closure is taken out of
/// `Mutex<Option<...>>` exactly once via `.take()`, and `Mutex<T>: Sync`
/// already holds when `T: Send`. Matches the consumer-side `ConsumerServiceFn`.
type ProducerServiceFn<T> =
    Box<dyn FnOnce(crate::RuntimeContext, crate::Producer<T>) -> BoxFuture<'static, ()> + Send>;

/// Type-erased trait for records — the storage and lifecycle contract.
///
/// Allows storage of heterogeneous record types in a single collection
/// while maintaining type safety through downcast operations.
///
/// Since the 036 W2 split this trait carries only the storage/lifecycle
/// surface. The other capabilities live in dedicated traits, reachable from
/// any `dyn AnyRecord`:
/// - [`RecordIntrospect`] (supertrait) — graph/metadata introspection
/// - [`RecordMetricsReset`] (supertrait) — profiling/metrics counter resets
/// - [`JsonRecordAccess`] — JSON remote access, via [`AnyRecord::json_access`]
///
/// Consumers: `AimDbBuilder::build()` (validation, config-error draining,
/// typed downcasts via [`AnyRecordExt`]) and connectors applying the
/// remote-access security policy (`set_writable_erased`).
///
/// # Thread Safety Requirements
///
/// This trait requires both `Send` and `Sync` because:
/// - Records are stored in `Arc<Box<dyn AnyRecord>>` and shared across threads
/// - The router system needs to access records from multiple connector tasks
/// - Emitter needs to be `Send+Sync` to work in async contexts
/// - The `FnOnce` consumers are moved out during spawning, so they don't affect `Sync`
///
/// **BREAKING CHANGE (v0.2.0):** Added `Sync` bound to `AnyRecord` trait.
/// Record types must now be both `Send + Sync`. Types that were previously
/// `Send` but not `Sync` can no longer be used as records. This change enables:
/// - Concurrent access to records from multiple connector tasks
/// - Safe sharing of record metadata across thread boundaries
/// - Type-safe routing in the bidirectional connector system
///
/// **Migration:** If your record type `T` is not `Sync`, wrap non-`Sync` fields
/// in `Arc<Mutex<_>>` or `Arc<RwLock<_>>` to achieve interior mutability with
/// thread-safe sharing.
pub trait AnyRecord: RecordIntrospect + RecordMetricsReset + Send + Sync {
    /// Validates that the record has correct producer/consumer setup
    ///
    /// Rules: Must have exactly one producer and at least one consumer.
    fn validate(&self) -> Result<(), &'static str>;

    /// Returns self as Any for downcasting
    fn as_any(&self) -> &dyn Any;

    /// Returns self as mutable Any for downcasting
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Drains the configuration mistakes recorded during registration.
    ///
    /// Called by `AimDbBuilder::build()`, which fills in the record key and
    /// reports every finding via
    /// [`DbError::InvalidConfiguration`](crate::DbError::InvalidConfiguration).
    fn drain_config_errors(&mut self) -> Vec<crate::error::ConfigError>;

    /// Sets the writable flag for this record (type-erased)
    ///
    /// Used internally by the builder to apply security policy to records.
    fn set_writable_erased(&self, writable: bool);

    /// Returns the record's JSON remote-access surface, if it has one.
    ///
    /// This accessor is the single place the `remote-access` cfg-gate lives
    /// for consumers: they query the capability here instead of cfg-gating
    /// every call site. `TypedRecord` always returns `Some`; the runtime
    /// "configured with `.with_remote_access()`" checks stay inside the
    /// [`JsonRecordAccess`] methods.
    #[cfg(feature = "remote-access")]
    fn json_access(&self) -> Option<&dyn JsonRecordAccess> {
        None
    }
}

/// Graph and metadata introspection for type-erased records.
///
/// Supertrait of [`AnyRecord`], so every stored record exposes it and a
/// `dyn AnyRecord` can be upcast to `&dyn RecordIntrospect` where only
/// introspection is needed.
///
/// Consumers: `AimDbBuilder::build()` (link validation and the dependency
/// graph fed to [`crate::graph`]), the builder's inbound/outbound route
/// collection, and `AimDbInner::list_records` (remote introspection
/// metadata).
pub trait RecordIntrospect {
    /// Returns the number of registered outbound connectors
    fn outbound_connector_count(&self) -> usize;

    /// Gets the outbound connector links
    ///
    /// Returns outbound connector configuration list for spawning logic.
    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink];

    /// Get the inbound connector links for this record
    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink];

    /// Returns the number of registered consumers (tap observers)
    fn consumer_count(&self) -> usize;

    /// Returns whether a producer service is registered
    fn has_producer(&self) -> bool;

    /// Returns whether a buffer is configured
    fn has_buffer(&self) -> bool;

    /// Returns whether a transform is registered for this record
    fn has_transform(&self) -> bool;

    /// Returns how this record gets its values (for dependency graph construction)
    fn record_origin(&self) -> crate::graph::RecordOrigin;

    /// Returns the buffer type name and capacity (for dependency graph)
    ///
    /// Returns (buffer_type_name, optional_capacity).
    fn buffer_info(&self) -> (String, Option<usize>);

    /// Returns the transform input keys (if a transform is registered)
    fn transform_input_keys(&self) -> Option<Vec<String>>;

    /// Collects metadata for this record
    #[cfg(feature = "remote-access")]
    fn collect_metadata(
        &self,
        type_id: core::any::TypeId,
        key: crate::record_id::StringKey,
        id: crate::record_id::RecordId,
    ) -> crate::remote::RecordMetadata;
}

/// Type-erased JSON read/subscribe/write for remote access.
///
/// Internal to the remote-access protocol — application code reads values
/// via `record.latest()?.as_json()` instead. Obtained from a record through
/// [`AnyRecord::json_access`], which is where the `remote-access` cfg-gate
/// lives for consumers.
///
/// Consumers: `AimDbInner::try_latest_as_json` / `set_record_from_json`
/// (`record.get` / `record.set`), the AimX session dispatch (`record.subscribe`
/// value drain), and `remote::stream::stream_record_updates`.
#[cfg(feature = "remote-access")]
pub trait JsonRecordAccess {
    /// Returns JSON for type-erased remote access
    ///
    /// Used internally by remote access protocol. **Users should use `record.latest()?.as_json()`.**
    fn latest_json(&self) -> Option<serde_json::Value>;

    /// Subscribe to record updates as JSON stream
    ///
    /// Creates a type-erased subscription that emits `serde_json::Value` instead of
    /// the concrete type `T`. This enables subscribing to a record without knowing
    /// its type at compile time.
    ///
    /// Used internally by remote access protocol for `record.subscribe` functionality.
    ///
    /// # Returns
    /// - `Ok(Box<dyn JsonBufferReader>)` - Successfully created subscription
    /// - `Err(DbError)` - If serialization not configured or buffer subscription failed
    ///
    /// # Errors
    /// Returns error if:
    /// - Record not configured with `.with_remote_access()`
    /// - Buffer subscription fails (shouldn't happen in practice)
    ///
    /// # Example (internal use)
    /// ```rust,ignore
    /// let record: &Box<dyn AnyRecord> = db.storage(id)?;
    /// let mut json_reader = record.json_access().unwrap().subscribe_json()?;
    ///
    /// while let Ok(json_val) = json_reader.recv_json().await {
    ///     // Forward to remote client...
    /// }
    /// ```
    fn subscribe_json(&self) -> crate::DbResult<Box<dyn crate::buffer::JsonBufferReader + Send>>;

    /// Sets a record value from JSON
    ///
    /// Deserializes JSON and produces the value to the record's buffer.
    ///
    /// **SAFETY:** This method enforces the "No Producer Override" rule:
    /// - Returns error if `producer_count > 0` (prevents overriding application logic)
    /// - Only configuration records (no producers) should be settable via remote access
    ///
    /// Used internally by remote access protocol for `record.set` functionality.
    ///
    /// # Arguments
    /// * `json_value` - JSON representation of the value to set
    ///
    /// # Returns
    /// - `Ok(())` - Successfully set the value
    /// - `Err(DbError)` - If deserialization fails, producers exist, or no buffer
    ///
    /// # Errors
    /// Returns error if:
    /// - Record has active producers (`producer_count > 0`) - **safety check**
    /// - JSON deserialization fails (schema mismatch)
    /// - Record not configured with buffer
    /// - Record not configured with `.with_remote_access()`
    ///
    /// # Example (internal use)
    /// ```rust,ignore
    /// let record: &Box<dyn AnyRecord> = db.storage(id)?;
    /// let json_val = serde_json::json!({"log_level": "debug"});
    /// // Only works if producer_count == 0
    /// record.json_access().unwrap().set_from_json(json_val)?;
    /// ```
    fn set_from_json(&self, json_value: serde_json::Value) -> crate::DbResult<()>;
}

/// Observability counter resets (features `profiling` / `metrics`).
///
/// Supertrait of [`AnyRecord`] with no-op defaults, so the cfg-gated reset
/// methods stay off the core storage contract while remaining callable on
/// every stored record.
///
/// Consumers: `AimDb::reset_profiling` / `AimDb::reset_buffer_metrics`,
/// driven by the AimX `control.reset_buffer_metrics` RPC and the MCP
/// buffer-metrics tool.
pub trait RecordMetricsReset {
    /// Resets this record's stage profiling counters (feature `profiling`).
    ///
    /// Default implementation is a no-op; `TypedRecord` overrides it.
    #[cfg(feature = "profiling")]
    fn reset_profiling(&self) {}

    /// Resets this record's buffer introspection counters (feature `metrics`).
    ///
    /// Default implementation is a no-op; `TypedRecord` overrides it.
    #[cfg(feature = "metrics")]
    fn reset_buffer_metrics(&self) {}
}

// Helper extension trait for type-safe downcasting
pub trait AnyRecordExt {
    /// Attempts to downcast to a typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>>;

    /// Attempts to downcast to a mutable typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&mut TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>>;
}

impl AnyRecordExt for Box<dyn AnyRecord> {
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>> {
        self.as_any().downcast_ref::<TypedRecord<T>>()
    }

    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>> {
        self.as_any_mut().downcast_mut::<TypedRecord<T>>()
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
    /// a pre-resolved handle to the same buffer — design 029 hot-path change.
    buffer: Option<Arc<dyn DynBuffer<T>>>,

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

    /// Per-stage profiling metrics (feature `profiling`).
    /// Stages are appended here in the same order they are registered on the
    /// `RecordRegistrar`, which matches the order the spawn machinery iterates them.
    #[cfg(feature = "profiling")]
    profiling: RecordProfilingMetrics,

    /// Whether this record allows writes via remote access (feature
    /// `remote-access`). Interior-mutable so the security policy can mark it after
    /// `build()`; read by `collect_metadata` into `RecordMetadata.writable`.
    #[cfg(feature = "remote-access")]
    writable: portable_atomic::AtomicBool,

    /// Type-erased JSON value codec (feature `json-serialize`).
    /// `Some` iff the record opted into JSON via `.with_remote_access()`.
    /// `RecordValue::as_json` and — under `remote-access` — the AimX read
    /// (`latest_json`), write (`set_from_json`), and subscribe (`subscribe_json`)
    /// paths route through it. Built from a `SerdeJsonCodec` where the
    /// `T: RemoteSerialize` bound is known at the call site.
    #[cfg(feature = "json-serialize")]
    remote_codec: Option<RecordCodec<T>>,

    /// Configuration mistakes recorded during registration instead of
    /// panicking (issue #133). Drained by `AimDbBuilder::build()`, which fills
    /// in the record key and reports all of them via
    /// [`DbError::InvalidConfiguration`](crate::DbError::InvalidConfiguration).
    config_errors: Vec<crate::error::ConfigError>,
}

impl<T: Send + 'static + Debug + Clone> TypedRecord<T> {
    /// Creates a new empty typed record
    ///
    /// Call `.with_remote_access()` to enable JSON (std only).
    pub fn new() -> Self {
        Self {
            producer: Mutex::new(None),
            consumers: Mutex::new(Vec::new()),
            transform: Mutex::new(None),
            buffer: None,
            buffer_cfg: None,
            outbound_connectors: Vec::new(),
            inbound_connectors: Vec::new(),
            #[cfg(feature = "profiling")]
            profiling: RecordProfilingMetrics::new(),
            #[cfg(feature = "remote-access")]
            writable: portable_atomic::AtomicBool::new(false),
            #[cfg(feature = "json-serialize")]
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

    /// Stage profiling metrics for this record (feature `profiling`).
    #[cfg(feature = "profiling")]
    pub fn profiling(&self) -> &RecordProfilingMetrics {
        &self.profiling
    }

    /// Mutable access to the stage profiling metrics — used during registration
    /// to append per-stage entries and assign names.
    #[cfg(feature = "profiling")]
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
        // Check for existing transform (mutual exclusion)
        if lock(&self.transform).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "Record already has a .transform(); cannot also have a .source().",
            ));
            return;
        }

        if !self.inbound_connectors.is_empty() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "Record already has a .link_from(); cannot also have a .source().",
            ));
            return;
        }

        // Check if already set
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
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// record.add_consumer(|ctx, consumer| async move {
    ///     let mut rx = consumer.subscribe();
    ///     while let Ok(value) = rx.recv().await {
    ///         println!("Consumer: {:?}", value);
    ///     }
    /// });
    /// ```
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
    /// and would race as last-writer-wins. Violations (including a second
    /// transform) are recorded — not panicked — and reported from `build()`;
    /// the conflicting registration is skipped.
    pub(crate) fn set_transform(&mut self, descriptor: crate::transform::TransformDescriptor<T>) {
        // Enforce mutual exclusion with .source()
        if lock(&self.producer).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "Record already has a .source(); cannot also have a .transform().",
            ));
            return;
        }

        if !self.inbound_connectors.is_empty() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                None,
                "Record already has a .link_from(); cannot also have a .transform().",
            ));
            return;
        }

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
            if input_keys.len() == 1 {
                return crate::graph::RecordOrigin::Transform {
                    input: input_keys.into_iter().next().unwrap(),
                };
            } else {
                return crate::graph::RecordOrigin::TransformJoin { inputs: input_keys };
            }
        }

        // Check for inbound connector (link)
        if let Some(first_link) = self.inbound_connectors.first() {
            return crate::graph::RecordOrigin::Link {
                protocol: first_link.url.scheme.clone(),
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
            let producer = crate::typed_api::Producer::new(self.writer_handle());

            let collected = (desc.build_fn)(producer, db.clone(), record_key);

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
    pub fn set_buffer(&mut self, buffer: Box<dyn DynBuffer<T>>) {
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
    /// `Some(&dyn DynBuffer<T>)` if buffer is set, `None` otherwise
    pub fn buffer(&self) -> Option<&dyn DynBuffer<T>> {
        self.buffer.as_deref()
    }

    /// Returns a fresh `Arc<dyn WriteHandle<T>>` bound to this record's buffer.
    /// Used at build time by the spawn machinery to pre-resolve `Producer<T>`
    /// handles (design 029).
    pub(crate) fn writer_handle(&self) -> Arc<dyn crate::buffer::WriteHandle<T>>
    where
        T: Send + Clone + 'static,
    {
        Arc::new(crate::buffer::RecordWriter::new(self.buffer.clone()))
    }

    /// Returns a clone of the buffer `Arc` (or `None` if no buffer is
    /// configured). Used at build time to pre-resolve `Consumer<T>` handles.
    pub(crate) fn buffer_handle(&self) -> Option<Arc<dyn DynBuffer<T>>> {
        self.buffer.clone()
    }

    /// Subscribes to the buffer for this record type
    ///
    /// # Errors
    /// Returns `DbError::MissingConfiguration` if no buffer configured
    pub fn subscribe(&self) -> crate::DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>> {
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| crate::DbError::missing_configuration("buffer"))?;

        Ok(buffer.subscribe_boxed())
    }

    /// Adds an outbound connector link for external system integration
    ///
    /// Bridges records to external protocols (MQTT, Kafka, HTTP, etc.).
    /// Multiple connectors supported per record.
    pub fn add_outbound_connector(&mut self, link: crate::connector::ConnectorLink) {
        self.outbound_connectors.push(link);
    }

    /// Installs the JSON value codec for this record (feature `json-serialize`)
    ///
    /// Enables `record.latest()?.as_json()` everywhere, and — under the
    /// `remote-access` feature — the AimX protocol (`record.get` / `set` /
    /// `subscribe`). Requires `json-serialize` and `T: RemoteSerialize`
    /// (blanket-impl'd for every `Serialize + DeserializeOwned` type). Works on
    /// no_std + alloc.
    #[cfg(feature = "json-serialize")]
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

    /// Returns a reference to the registered outbound connectors
    ///
    /// # Returns
    /// A slice of outbound connector links
    pub fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.outbound_connectors
    }

    /// Returns the number of registered outbound connectors
    ///
    /// # Returns
    /// The count of outbound connectors
    pub fn outbound_connector_count(&self) -> usize {
        self.outbound_connectors.len()
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
        if lock(&self.producer).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                Some(alloc::format!("{}", link.url)),
                "Record already has a .source(); cannot also have a .link_from().",
            ));
            return;
        }

        if lock(&self.transform).is_some() {
            self.push_config_error(crate::error::ConfigError::new(
                "",
                Some(alloc::format!("{}", link.url)),
                "Record already has a .transform(); cannot also have a .link_from().",
            ));
            return;
        }

        self.inbound_connectors.push(link);
    }

    /// Returns the number of registered consumers
    ///
    /// # Returns
    /// The count of consumers
    pub fn consumer_count(&self) -> usize {
        lock(&self.consumers).len()
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
        #[cfg(feature = "profiling")]
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

        #[cfg_attr(not(feature = "profiling"), allow(unused_variables))]
        for (i, consumer_fn) in consumers.into_iter().enumerate() {
            // Create a Consumer<T> bound to a pre-resolved buffer handle.
            #[allow(unused_mut)]
            let mut consumer = crate::typed_api::Consumer::new(buffer_arc.clone());

            #[cfg(feature = "profiling")]
            if let Some(entry) = self.profiling.tap(i) {
                consumer.set_profiling(entry.metrics.clone(), db.profiling_clock().clone());
            }

            futures.push(consumer_fn(db.runtime_ctx(), consumer));
        }

        Ok(futures)
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
            #[allow(unused_mut)]
            let mut producer = crate::typed_api::Producer::new(self.writer_handle());

            #[cfg(feature = "profiling")]
            if let Some(entry) = self.profiling.source(0) {
                producer.set_profiling(entry.metrics.clone(), db.profiling_clock().clone());
            }

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
    #[cfg(feature = "remote-access")]
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
    ///
    /// # Examples
    /// ```rust,ignore
    /// // Direct access (std and no_std)
    /// if let Some(value) = record.latest() {
    ///     println!("Temp: {:.1}°C", value.celsius);
    /// }
    ///
    /// // JSON serialization (std only)
    /// if let Some(json) = record.latest()?.as_json() {
    ///     println!("{}", json);
    /// }
    /// ```
    pub fn latest(&self) -> Option<RecordValue<T>> {
        // Read buffer-native storage via peek() (design 031). Records without
        // a buffer return None — see Breaking Changes in design 031.
        let value = self.buffer.as_ref()?.peek()?;
        #[cfg(feature = "json-serialize")]
        {
            Some(RecordValue::new(value, self.remote_codec.clone()))
        }
        #[cfg(not(feature = "json-serialize"))]
        {
            Some(RecordValue::new(value, None))
        }
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
    fn validate(&self) -> Result<(), &'static str> {
        // Producer service is optional - some records are driven by external events
        // Consumer is also optional - records can be accessed via:
        // - Explicit consumers (tap, link)
        // - Remote access (AimX protocol)
        // - Direct producer/consumer API

        // A remote-access record has no fallback storage since design 031
        // removed latest_snapshot: reads/writes go straight to the buffer. With
        // no buffer, `record.get`/`latest()` return not_found and `record.set`
        // silently discards the value. Fail at build() so the buffer is added
        // explicitly instead of surfacing as a silent runtime no-op.
        #[cfg(feature = "json-serialize")]
        if self.remote_codec.is_some() && !self.has_buffer() {
            return Err("record has .with_remote_access() but no buffer; \
                 add a buffer (e.g. .buffer(BufferCfg::SingleLatest)) so record.get/set work");
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn drain_config_errors(&mut self) -> Vec<crate::error::ConfigError> {
        core::mem::take(&mut self.config_errors)
    }

    fn set_writable_erased(&self, writable: bool) {
        #[cfg(feature = "remote-access")]
        {
            self.writable
                .store(writable, portable_atomic::Ordering::SeqCst);
        }
        #[cfg(not(feature = "remote-access"))]
        {
            let _ = writable; // Suppress unused warning
        }
    }

    #[cfg(feature = "remote-access")]
    fn json_access(&self) -> Option<&dyn JsonRecordAccess> {
        Some(self)
    }
}

impl<T: Send + Sync + 'static + Debug + Clone> RecordIntrospect for TypedRecord<T> {
    fn outbound_connector_count(&self) -> usize {
        self.outbound_connectors.len()
    }

    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.outbound_connectors
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

    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink] {
        &self.inbound_connectors
    }

    #[cfg(feature = "remote-access")]
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
            self.outbound_connector_count(),
        );

        // Add buffer metrics if available
        #[cfg(feature = "metrics")]
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
        #[cfg(feature = "profiling")]
        let metadata = metadata.with_stage_profiling(self.profiling.snapshot());

        metadata
    }
}

#[cfg(feature = "remote-access")]
impl<T: Send + Sync + 'static + Debug + Clone> JsonRecordAccess for TypedRecord<T> {
    fn latest_json(&self) -> Option<serde_json::Value> {
        log_debug!(
            "latest_json called for type: {}",
            core::any::type_name::<T>()
        );

        // Read buffer-native storage via peek() (design 031). Records without
        // a buffer return None — see Breaking Changes in design doc.
        let value = self.buffer.as_ref()?.peek()?;
        let result = self.remote_codec.as_ref()?.encode(&value);

        log_debug!("Serialization result: {:?}", result.is_some());

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

        // 2. Subscribe to the buffer (get Box<dyn BufferReader<T>>)
        let reader = self.subscribe()?;

        // 3. Wrap in JsonReaderAdapter
        let json_reader = JsonReaderAdapter {
            inner: reader,
            codec,
        };

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

        // SAFETY CHECK 1: Enforce "No Producer Override" rule
        if self.has_producer() || self.has_transform() {
            log_warn!(
                "Rejected set_from_json for '{}': has active producer or transform",
                core::any::type_name::<T>()
            );

            return Err(DbError::permission_denied(alloc::format!(
                "Cannot set record '{}' - has active producer or transform. \
                 Use internal application logic instead. \
                 Remote access can only set configuration records without producers.",
                core::any::type_name::<T>()
            )));
        }

        // Check if the codec is configured (set by .with_remote_access())
        let codec = self.remote_codec.clone().ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Record '{}' not configured with .with_remote_access(). \
                 Cannot deserialize from JSON.",
                core::any::type_name::<T>()
            ))
        })?;

        // Check if buffer exists
        if self.buffer.is_none() {
            return Err(DbError::runtime_error(alloc::format!(
                "Record '{}' has no buffer configured. \
                 Cannot produce value without buffer.",
                core::any::type_name::<T>()
            )));
        }

        // Deserialize JSON -> T
        let value: T = codec.decode(&json_value).ok_or_else(|| {
            DbError::runtime_error(alloc::format!(
                "Failed to deserialize JSON to type '{}'. \
                 JSON structure does not match the expected schema.",
                core::any::type_name::<T>()
            ))
        })?;

        log_debug!(
            "Successfully deserialized JSON to type: {}",
            core::any::type_name::<T>()
        );

        // Push through the unified write path (design 031). This also marks
        // metadata as updated — previously skipped on this path.
        self.writer_handle().push(value);

        log_info!(
            "Successfully set value from JSON for record: {}",
            core::any::type_name::<T>()
        );

        Ok(())
    }
}

impl<T: Send + Sync + 'static + Debug + Clone> RecordMetricsReset for TypedRecord<T> {
    #[cfg(feature = "profiling")]
    fn reset_profiling(&self) {
        self.profiling.reset_all();
    }

    #[cfg(feature = "metrics")]
    fn reset_buffer_metrics(&self) {
        if let Some(buf) = &self.buffer {
            buf.reset_metrics();
        }
    }
}
