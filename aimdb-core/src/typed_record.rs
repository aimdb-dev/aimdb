//! Type-safe record storage using TypeId
//!
//! Provides type-safe record identification using Rust's `TypeId` for compile-time safety.
//!
//! # Feature Support
//!
//! **Both std and no_std**: Core API (`TypedRecord`, `latest()`, `RecordValue`, producer/consumer)
//!
//! **std only**: JSON serialization (`.with_remote_access()`, `.as_json()`), remote access, metadata
//!
//! **no_std**: Use `record.latest()` for value access and `Deref` for fields. JSON requires std;
//! implement custom serialization for embedded protocols (CBOR, MessagePack, etc.).

use core::any::Any;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

#[cfg(not(feature = "std"))]
use alloc::string::ToString;

#[cfg(feature = "std")]
use std::{boxed::Box, string::String, sync::Arc, vec::Vec};

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

/// Type alias for JSON serializer function (std only)
#[cfg(feature = "std")]
type JsonSerializer<T> = Arc<dyn Fn(&T) -> Option<serde_json::Value> + Send + Sync>;

/// Type alias for JSON deserializer function (std only)
#[cfg(feature = "std")]
type JsonDeserializer<T> = Arc<dyn Fn(&serde_json::Value) -> Option<T> + Send + Sync>;

/// Wrapper for a record's latest value with optional serialization
///
/// Created by `TypedRecord::latest()`. Core methods (`get()`, `into_inner()`, `Deref`) work in
/// both std and no_std. JSON serialization (`.as_json()`) requires std feature.
pub struct RecordValue<T> {
    value: T,
    #[cfg(feature = "std")]
    serializer: Option<JsonSerializer<T>>,
}

impl<T> RecordValue<T> {
    /// Create a new RecordValue with optional serializer
    #[cfg(feature = "std")]
    fn new(value: T, serializer: Option<JsonSerializer<T>>) -> Self {
        Self { value, serializer }
    }

    /// Create a new RecordValue without serializer (no_std)
    #[cfg(not(feature = "std"))]
    fn new(value: T, _serializer: Option<()>) -> Self {
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

    /// Serialize the value to JSON (std only)
    ///
    /// Returns `Some(JsonValue)` if record was configured with `.with_remote_access()`,
    /// otherwise `None`. Requires `serde_json` (std only). For no_std, use `.get()`,
    /// `.into_inner()`, or `Deref` for direct access.
    #[cfg(feature = "std")]
    pub fn as_json(&self) -> Option<serde_json::Value> {
        let serializer = self.serializer.as_ref()?;
        serializer(&self.value)
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
#[cfg(feature = "std")]
struct JsonReaderAdapter<T: Clone + Send + 'static> {
    /// The underlying typed buffer reader
    inner: Box<dyn crate::buffer::BufferReader<T> + Send>,
    /// JSON serializer function (from .with_remote_access())
    serializer: JsonSerializer<T>,
}

#[cfg(feature = "std")]
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
            (self.serializer)(&value).ok_or_else(|| {
                #[cfg(feature = "std")]
                {
                    crate::DbError::RuntimeError {
                        message: "Failed to serialize value to JSON".to_string(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    crate::DbError::RuntimeError { _message: () }
                }
            })
        })
    }

    fn try_recv_json(&mut self) -> Result<serde_json::Value, crate::DbError> {
        // Non-blocking receive from underlying typed buffer
        let value = self.inner.try_recv()?;

        // Serialize to JSON using the configured serializer
        (self.serializer)(&value).ok_or_else(|| crate::DbError::RuntimeError {
            message: "Failed to serialize value to JSON".to_string(),
        })
    }
}

/// Metadata tracking for records (std only - used for remote access introspection)
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub(crate) struct RecordMetadataTracker {
    /// Human-readable record name (type name)
    name: String,
    /// Creation timestamp (seconds, nanoseconds since UNIX_EPOCH)
    created_at: (u64, u32),
    /// Last update timestamp (seconds, nanoseconds since UNIX_EPOCH, None if never updated)
    last_update: std::sync::Arc<std::sync::Mutex<Option<(u64, u32)>>>,
    /// Whether this record allows writes via remote access (using interior mutability)
    writable: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "std")]
impl RecordMetadataTracker {
    fn new<T: 'static>() -> Self {
        use std::time::SystemTime;

        let duration = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        Self {
            name: core::any::type_name::<T>().to_string(),
            created_at: (duration.as_secs(), duration.subsec_nanos()),
            last_update: Arc::new(std::sync::Mutex::new(None)),
            writable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub(crate) fn mark_updated(&self) {
        use std::time::SystemTime;

        let duration = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        if let Ok(mut last) = self.last_update.lock() {
            *last = Some((duration.as_secs(), duration.subsec_nanos()));
        }
    }

    fn set_writable(&self, writable: bool) {
        self.writable
            .store(writable, std::sync::atomic::Ordering::SeqCst);
    }

    /// Formats a Unix timestamp as "secs.nanosecs" string
    fn format_timestamp(timestamp: (u64, u32)) -> String {
        format!("{}.{:09}", timestamp.0, timestamp.1)
    }
}

// Type alias for boxed futures
pub(crate) type BoxFuture<'a, T> =
    core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;

/// Type alias for consumer service closure stored in TypedRecord
/// Each consumer receives a Consumer<T> handle for subscribing to the buffer
/// and a runtime context (Arc<dyn Any>) for accessing runtime capabilities
type ConsumerServiceFn<T> = Box<
    dyn FnOnce(crate::Consumer<T>, Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, ()> + Send,
>;

/// Type alias for producer service closure stored in TypedRecord
/// Takes (Producer<T>, RuntimeContext) and returns a Future
/// This will be auto-spawned during build().
/// `Send` only — `Sync` is not required because the closure is taken out of
/// `Mutex<Option<...>>` exactly once via `.take()`, and `Mutex<T>: Sync`
/// already holds when `T: Send`. Matches the consumer-side `ConsumerServiceFn`.
type ProducerServiceFn<T> = Box<
    dyn FnOnce(crate::Producer<T>, Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, ()> + Send,
>;

/// Type-erased trait for records
///
/// Allows storage of heterogeneous record types in a single collection
/// while maintaining type safety through downcast operations.
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
pub trait AnyRecord: Send + Sync {
    /// Validates that the record has correct producer/consumer setup
    ///
    /// Rules: Must have exactly one producer and at least one consumer.
    fn validate(&self) -> Result<(), &'static str>;

    /// Returns self as Any for downcasting
    fn as_any(&self) -> &dyn Any;

    /// Returns self as mutable Any for downcasting
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Returns the number of registered outbound connectors
    fn outbound_connector_count(&self) -> usize;

    /// Returns the outbound connector URLs as strings
    #[cfg(feature = "std")]
    fn outbound_connector_urls(&self) -> Vec<String>;

    /// Gets the outbound connector links
    ///
    /// Returns outbound connector configuration list for spawning logic.
    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink];

    /// Returns the number of registered consumers (tap observers)
    fn consumer_count(&self) -> usize;

    /// Returns whether a producer service is registered
    fn has_producer_service(&self) -> bool;

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

    /// Sets the writable flag for this record (type-erased)
    ///
    /// Used internally by the builder to apply security policy to records.
    fn set_writable_erased(&self, writable: bool);

    /// Collects metadata for this record (std only)
    #[cfg(feature = "std")]
    fn collect_metadata(
        &self,
        type_id: core::any::TypeId,
        key: crate::record_id::StringKey,
        id: crate::record_id::RecordId,
    ) -> crate::remote::RecordMetadata;

    /// Internal: Returns JSON for type-erased remote access (std only)
    ///
    /// Used internally by remote access protocol. **Users should use `record.latest()?.as_json()`.**
    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn latest_json(&self) -> Option<serde_json::Value>;

    /// Subscribe to record updates as JSON stream (std only)
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
    /// let type_id = TypeId::of::<Temperature>();
    /// let record: &Box<dyn AnyRecord> = db.records.get(&type_id)?;
    /// let mut json_reader = record.subscribe_json()?;
    ///
    /// while let Ok(json_val) = json_reader.recv_json().await {
    ///     // Forward to remote client...
    /// }
    /// ```
    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn subscribe_json(&self) -> crate::DbResult<Box<dyn crate::buffer::JsonBufferReader + Send>>;

    /// Sets a record value from JSON (std only)
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
    /// let type_id = TypeId::of::<AppConfig>();
    /// let record: &Box<dyn AnyRecord> = db.records.get(&type_id)?;
    /// let json_val = serde_json::json!({"log_level": "debug"});
    /// record.set_from_json(json_val)?; // Only works if producer_count == 0
    /// ```
    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn set_from_json(&self, json_value: serde_json::Value) -> crate::DbResult<()>;

    /// Get the inbound connector links for this record
    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink];

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
    /// * `R` - The runtime type
    ///
    /// # Returns
    /// `Some(&TypedRecord<T, R>)` if types match, `None` otherwise
    fn as_typed<T: Send + 'static + Debug + Clone, R: aimdb_executor::RuntimeAdapter + 'static>(
        &self,
    ) -> Option<&TypedRecord<T, R>>;

    /// Attempts to downcast to a mutable typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    /// * `R` - The runtime type
    ///
    /// # Returns
    /// `Some(&mut TypedRecord<T, R>)` if types match, `None` otherwise
    fn as_typed_mut<
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::RuntimeAdapter + 'static,
    >(
        &mut self,
    ) -> Option<&mut TypedRecord<T, R>>;
}

impl AnyRecordExt for Box<dyn AnyRecord> {
    fn as_typed<T: Send + 'static + Debug + Clone, R: aimdb_executor::RuntimeAdapter + 'static>(
        &self,
    ) -> Option<&TypedRecord<T, R>> {
        self.as_any().downcast_ref::<TypedRecord<T, R>>()
    }

    fn as_typed_mut<
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::RuntimeAdapter + 'static,
    >(
        &mut self,
    ) -> Option<&mut TypedRecord<T, R>> {
        self.as_any_mut().downcast_mut::<TypedRecord<T, R>>()
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
    /// Downcasts the type-erased `AnyRecord` to `TypedRecord<T, R>` then returns
    /// every future the record contributes to the runner. Source/transform are
    /// mutually exclusive — at most one will appear.
    pub fn collect_all_futures<R>(
        record: &dyn AnyRecord,
        runtime: &Arc<R>,
        db: &Arc<crate::builder::AimDb<R>>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>>
    where
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        use crate::DbError;

        // Downcast to TypedRecord<T, R>
        let typed_record: &TypedRecord<T, R> =
            record.as_any().downcast_ref::<TypedRecord<T, R>>().ok_or({
                #[cfg(feature = "std")]
                {
                    DbError::RecordNotFound {
                        record_name: core::any::type_name::<T>().to_string(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    DbError::RecordNotFound { _record_name: () }
                }
            })?;

        let mut futures = Vec::new();

        if typed_record.has_producer_service() {
            if let Some(f) = typed_record.collect_producer_future(runtime, db, record_key)? {
                futures.push(f);
            }
        }

        if typed_record.has_transform() {
            futures.extend(typed_record.collect_transform_futures(runtime, db, record_key)?);
        }

        if typed_record.consumer_count() > 0 {
            futures.extend(typed_record.collect_consumer_futures(runtime, db, record_key)?);
        }

        Ok(futures)
    }
}

/// Typed record storage with producer/consumer functions
///
/// Stores type-safe producer and consumer functions with optional buffering for async dispatch.
pub struct TypedRecord<
    T: Send + 'static + Debug + Clone,
    R: aimdb_executor::RuntimeAdapter + 'static,
> {
    /// Optional producer service - a task that generates data
    /// This will be auto-spawned during build() if present
    /// Stored as FnOnce that takes (Producer<T>, RuntimeContext) and returns a Future
    /// Wrapped in Mutex for interior mutability (needed to take() during spawning)
    producer_service: Mutex<Option<ProducerServiceFn<T>>>,

    /// List of consumer/tap tasks - wrapped in Mutex for Sync + taking out during spawn
    /// Each is spawned as an independent background task that subscribes to the buffer
    /// Using Mutex provides the Sync bound required by AnyRecord trait
    consumers: Mutex<Vec<ConsumerServiceFn<T>>>,

    /// Transform descriptor — mutually exclusive with producer_service.
    /// If set, this record is a reactive derivation from one or more input records.
    /// Uses the same Mutex pattern for take()-during-spawn.
    transform: Mutex<Option<crate::transform::TransformDescriptor<T, R>>>,

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

    /// Metadata tracking (std only - for remote access)
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,

    /// JSON serializer function (std only - for remote access)
    /// When set via .with_remote_access(), automatically serializes values for record.get queries
    /// Stores the serialization logic where T: Serialize is known at call site
    #[cfg(feature = "std")]
    json_serializer: Option<JsonSerializer<T>>,

    /// JSON deserializer function (std only - for remote access)
    /// When set via .with_remote_access(), automatically deserializes JSON for record.set operations
    /// Stores the deserialization logic where T: Deserialize is known at call site
    #[cfg(feature = "std")]
    json_deserializer: Option<JsonDeserializer<T>>,
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::RuntimeAdapter + 'static>
    TypedRecord<T, R>
{
    /// Creates a new empty typed record
    ///
    /// Call `.with_remote_access()` to enable JSON (std only).
    pub fn new() -> Self {
        Self {
            producer_service: Mutex::new(None),
            consumers: Mutex::new(Vec::new()),
            transform: Mutex::new(None),
            buffer: None,
            buffer_cfg: None,
            outbound_connectors: Vec::new(),
            inbound_connectors: Vec::new(),
            #[cfg(feature = "profiling")]
            profiling: RecordProfilingMetrics::new(),
            #[cfg(feature = "std")]
            metadata: RecordMetadataTracker::new::<T>(),
            #[cfg(feature = "std")]
            json_serializer: None,
            #[cfg(feature = "std")]
            json_deserializer: None,
        }
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
    /// # Panics
    /// Panics if producer already set (one producer per record), if a transform is registered,
    /// or if a `.link_from()` inbound connector is registered (all three would race on the buffer).
    pub fn set_producer_service<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::Producer<T>, Arc<dyn Any + Send + Sync>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Check for existing transform (mutual exclusion)
        if lock(&self.transform).is_some() {
            panic!("Record already has a .transform(); cannot also have a .source().");
        }

        if !self.inbound_connectors.is_empty() {
            panic!("Record already has a .link_from(); cannot also have a .source().");
        }

        // Check if already set
        if lock(&self.producer_service).is_some() {
            panic!("This record type already has a producer service");
        }

        // Box the future-returning function
        let boxed_fn = Box::new(
            move |producer: crate::Producer<T>,
                  ctx: Arc<dyn Any + Send + Sync>|
                  -> BoxFuture<'static, ()> { Box::pin(f(producer, ctx)) },
        );

        // Store it in the mutex
        *lock(&self.producer_service) = Some(boxed_fn);
    }

    /// Adds a consumer function for this record
    ///
    /// Consumer functions are spawned as independent background tasks that
    /// subscribe to the buffer and process values asynchronously.
    /// Multiple consumers can be registered for the same record type.
    ///
    /// # Arguments
    /// * `f` - A function that takes `Consumer<T>`, runtime context, and returns a Future
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// record.add_consumer(|consumer, ctx_any| async move {
    ///     let mut rx = consumer.subscribe();
    ///     while let Ok(value) = rx.recv().await {
    ///         println!("Consumer: {:?}", value);
    ///     }
    /// });
    /// ```
    pub fn add_consumer<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::Consumer<T>, Arc<dyn Any + Send + Sync>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Box the future to make it trait object compatible
        let boxed = Box::new(
            move |consumer: crate::Consumer<T>,
                  ctx_any: Arc<dyn Any + Send + Sync>|
                  -> BoxFuture<'static, ()> { Box::pin(f(consumer, ctx_any)) },
        );

        lock(&self.consumers).push(boxed);
    }

    /// Sets the transform descriptor for this record.
    ///
    /// A transform is mutually exclusive with `.source()` and with any
    /// `.link_from()` inbound connector — all three write to the same buffer
    /// and would race as last-writer-wins. Panics if any of those are
    /// already registered, or if a transform is already set.
    pub(crate) fn set_transform(
        &mut self,
        descriptor: crate::transform::TransformDescriptor<T, R>,
    ) {
        // Enforce mutual exclusion with .source()
        if lock(&self.producer_service).is_some() {
            panic!("Record already has a .source(); cannot also have a .transform().");
        }

        if !self.inbound_connectors.is_empty() {
            panic!("Record already has a .link_from(); cannot also have a .transform().");
        }

        let mut slot = lock(&self.transform);
        if slot.is_some() {
            panic!("Record already has a .transform(); only one is allowed.");
        }
        *slot = Some(descriptor);
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
        if lock(&self.producer_service).is_some() {
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
        runtime: &Arc<R>,
        db: &Arc<crate::AimDb<R>>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>>
    where
        R: aimdb_executor::RuntimeAdapter,
        T: Sync,
    {
        // Take the transform descriptor (can only collect once)
        let descriptor = lock(&self.transform).take();

        if let Some(desc) = descriptor {
            #[cfg(feature = "tracing")]
            tracing::info!(
                "🔄 Collecting transform futures for '{}' (inputs: {:?})",
                record_key,
                desc.input_keys
            );

            // Create Producer<T> bound to a pre-resolved write handle for this record.
            let producer = crate::typed_api::Producer::new(self.writer_handle());

            let collected = (desc.build_fn)(producer, db.clone(), runtime.clone(), record_key);

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

    /// Returns a fresh `Arc<dyn WriteHandle<T>>` bound to this record's buffer,
    /// snapshot, and metadata. Used at build time by the spawn machinery to
    /// pre-resolve `Producer<T>` handles (design 029).
    pub(crate) fn writer_handle(&self) -> Arc<dyn crate::buffer::WriteHandle<T>>
    where
        T: Send + Clone + 'static,
    {
        #[cfg(feature = "std")]
        {
            Arc::new(crate::buffer::RecordWriter::new(
                self.buffer.clone(),
                self.metadata.clone(),
            ))
        }
        #[cfg(not(feature = "std"))]
        {
            Arc::new(crate::buffer::RecordWriter::new(self.buffer.clone()))
        }
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
        let buffer = self.buffer.as_ref().ok_or({
            #[cfg(feature = "std")]
            {
                crate::DbError::MissingConfiguration {
                    parameter: "buffer".to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                crate::DbError::MissingConfiguration { _parameter: () }
            }
        })?;

        Ok(buffer.subscribe_boxed())
    }

    /// Adds an outbound connector link for external system integration
    ///
    /// Bridges records to external protocols (MQTT, Kafka, HTTP, etc.).
    /// Multiple connectors supported per record.
    pub fn add_outbound_connector(&mut self, link: crate::connector::ConnectorLink) {
        self.outbound_connectors.push(link);
    }

    /// Enables JSON serialization for remote access (std only)
    ///
    /// Enables `record.latest()?.as_json()` and remote access `record.get` protocol.
    /// Requires `std` feature and `T: serde::Serialize`.
    ///
    /// For no_std, use `record.latest()` for value access or custom serialization.
    #[cfg(feature = "std")]
    pub fn with_remote_access(&mut self) -> &mut Self
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        // Store serialization function where T: Serialize is known
        self.json_serializer = Some(std::sync::Arc::new(|val: &T| {
            serde_json::to_value(val).ok()
        }));

        // Store deserialization function where T: DeserializeOwned is known
        self.json_deserializer = Some(std::sync::Arc::new(|json_val: &serde_json::Value| {
            serde_json::from_value(json_val.clone()).ok()
        }));

        #[cfg(feature = "tracing")]
        tracing::info!(
            "with_remote_access() called for record type: {}",
            core::any::type_name::<T>()
        );

        self
    }

    /// Enables JSON serialization (backward compatible, read-only)
    ///
    /// This version only adds serialization support (for `record.get` and `record.subscribe`).
    /// For write support via `record.set`, use `.with_remote_access()` which requires
    /// both `Serialize` and `DeserializeOwned` bounds.
    ///
    /// This method exists for backward compatibility with records that don't implement
    /// `DeserializeOwned` but still need to be readable via remote access.
    #[cfg(feature = "std")]
    pub fn with_read_only_serialization(&mut self) -> &mut Self
    where
        T: serde::Serialize,
    {
        // Store only serialization function
        self.json_serializer = Some(std::sync::Arc::new(|val: &T| {
            serde_json::to_value(val).ok()
        }));

        // Deserialization intentionally left as None - will fail at runtime if
        // someone tries to use record.set on this record

        #[cfg(feature = "tracing")]
        tracing::info!(
            "with_read_only_serialization() called for record type: {}",
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
    /// # Panics
    /// Panics if a `.source()` or `.transform()` is already registered.
    /// All three write to the same buffer and would race as last-writer-wins.
    /// Multiple inbound connectors on the same record are permitted (fan-in).
    pub fn add_inbound_connector(&mut self, link: crate::connector::InboundConnectorLink) {
        if lock(&self.producer_service).is_some() {
            panic!("Record already has a .source(); cannot also have a .link_from().");
        }

        if lock(&self.transform).is_some() {
            panic!("Record already has a .transform(); cannot also have a .link_from().");
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
        runtime: &Arc<R>,
        db: &Arc<crate::AimDb<R>>,
        record_key: &str,
    ) -> crate::DbResult<Vec<BoxFuture<'static, ()>>>
    where
        R: aimdb_executor::RuntimeAdapter,
        T: Sync,
    {
        // `db` carries the profiling clock; `record_key` shows up in the no-buffer
        // error message (std only). Both are unused when their feature is off —
        // silence them narrowly so an unused `runtime` would still warn.
        #[cfg(not(feature = "profiling"))]
        let _ = db;
        #[cfg(not(feature = "std"))]
        let _ = record_key;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Collecting {} consumer futures for record type {}",
            self.consumer_count(),
            core::any::type_name::<T>()
        );

        // Take all consumers from the Mutex
        let consumers = core::mem::take(&mut *lock(&self.consumers));

        // Invariant: taps are pushed to `profiling` in the same order consumers are
        // added (both happen in `RecordRegistrar::tap_raw`), so index `i` lines up.
        #[cfg(feature = "profiling")]
        debug_assert_eq!(self.profiling.tap_count(), consumers.len());

        let mut futures = Vec::with_capacity(consumers.len());

        // Pre-resolve the buffer handle once — every consumer shares the same Arc.
        // A `.tap()` requires a buffer; surface the misconfiguration here with the
        // record key in the message rather than panicking inside `Consumer::new`.
        let buffer_arc = self.buffer_handle().ok_or({
            #[cfg(feature = "std")]
            {
                crate::DbError::MissingConfiguration {
                    parameter: alloc::format!(
                        "buffer for record '{}' (required by .tap())",
                        record_key
                    ),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                crate::DbError::MissingConfiguration { _parameter: () }
            }
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

            // Type-erase the runtime for the consumer signature.
            let runtime_any: Arc<dyn Any + Send + Sync> = runtime.clone();

            futures.push(consumer_fn(consumer, runtime_any));
        }

        Ok(futures)
    }

    /// Collects the producer-service future, if one is registered.
    pub fn collect_producer_future(
        &self,
        runtime: &Arc<R>,
        db: &Arc<crate::AimDb<R>>,
        record_key: &str,
    ) -> crate::DbResult<Option<BoxFuture<'static, ()>>>
    where
        R: aimdb_executor::RuntimeAdapter,
        T: Sync,
    {
        // `db` is only consulted under `profiling` for the clock; silence the
        // unused-variable warning narrowly so an unused `runtime` would still warn.
        // `record_key` is silenced inside the `if let Some(...)` branch below.
        #[cfg(not(feature = "profiling"))]
        let _ = db;

        // Take the producer service (can only collect once)
        let service = lock(&self.producer_service).take();

        if let Some(service_fn) = service {
            #[cfg(feature = "tracing")]
            tracing::debug!(
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

            let ctx: Arc<dyn core::any::Any + Send + Sync> = runtime.clone();

            Ok(Some(service_fn(producer, ctx)))
        } else {
            Ok(None)
        }
    }

    /// Returns whether a producer service is registered
    pub fn has_producer_service(&self) -> bool {
        lock(&self.producer_service).is_some()
    }

    /// Marks this record as writable for remote access (std only)
    #[cfg(feature = "std")]
    pub fn set_writable(&self, writable: bool) {
        self.metadata.set_writable(writable);
    }

    /// Returns the latest produced value
    ///
    /// Returns most recent value wrapped in `RecordValue<T>`, updated atomically on each `produce()`.
    /// Non-blocking and buffer-agnostic.
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
        #[cfg(feature = "std")]
        {
            Some(RecordValue::new(value, self.json_serializer.clone()))
        }
        #[cfg(not(feature = "std"))]
        {
            Some(RecordValue::new(value, None))
        }
    }

    /// Creates a boxed ProducerTrait for this record type (std only)
    ///
    /// Returns a type-erased producer that implements ProducerTrait,
    /// allowing inbound connectors to produce values without knowing the concrete type.
    ///
    /// Backed by a pre-resolved `WriteHandle` (design 029) — no db / key lookup
    /// is performed on each `produce_any` call.
    #[cfg(feature = "std")]
    pub fn create_producer_trait(&self) -> Box<dyn crate::connector::ProducerTrait>
    where
        T: Send + 'static + Debug + Clone,
    {
        Box::new(crate::typed_api::Producer::<T>::new(self.writer_handle()))
    }
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::RuntimeAdapter + 'static> Default
    for TypedRecord<T, R>
{
    fn default() -> Self {
        Self::new()
    }
}

// BREAKING CHANGE (v0.2.0): TypedRecord now requires T: Sync
// This enables safe concurrent access to records from multiple connector tasks
// in the bidirectional routing system. The Sync bound propagates from AnyRecord
// trait and ensures thread-safe sharing of record values.
impl<T: Send + Sync + 'static + Debug + Clone, R: aimdb_executor::RuntimeAdapter + 'static>
    AnyRecord for TypedRecord<T, R>
{
    fn validate(&self) -> Result<(), &'static str> {
        // Producer service is optional - some records are driven by external events
        // Consumer is also optional - records can be accessed via:
        // - Explicit consumers (tap, link)
        // - Remote access (AimX protocol)
        // - Direct producer/consumer API
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn outbound_connector_count(&self) -> usize {
        self.outbound_connectors.len()
    }

    #[cfg(feature = "std")]
    fn outbound_connector_urls(&self) -> Vec<String> {
        self.outbound_connectors
            .iter()
            .map(|link| format!("{}", link.url))
            .collect()
    }

    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.outbound_connectors
    }

    fn consumer_count(&self) -> usize {
        TypedRecord::consumer_count(self)
    }

    fn has_producer_service(&self) -> bool {
        TypedRecord::has_producer_service(self)
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

    fn set_writable_erased(&self, writable: bool) {
        #[cfg(feature = "std")]
        {
            self.metadata.set_writable(writable);
        }
        #[cfg(not(feature = "std"))]
        {
            let _ = writable; // Suppress unused warning
        }
    }

    #[cfg(feature = "std")]
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

        let last_update = self
            .metadata
            .last_update
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .map(RecordMetadataTracker::format_timestamp);

        let metadata = crate::remote::RecordMetadata::new(
            id,
            key,
            type_id,
            self.metadata.name.clone(),
            self.record_origin(),
            buffer_type,
            buffer_capacity,
            if self.has_producer_service() { 1 } else { 0 },
            self.consumer_count(),
            self.metadata
                .writable
                .load(std::sync::atomic::Ordering::SeqCst),
            RecordMetadataTracker::format_timestamp(self.metadata.created_at),
            self.outbound_connector_count(),
        )
        .with_last_update_opt(last_update);

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

    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn latest_json(&self) -> Option<serde_json::Value> {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "latest_json called for type: {}",
            core::any::type_name::<T>()
        );

        // Read buffer-native storage via peek() (design 031). Records without
        // a buffer return None — see Breaking Changes in design doc.
        let value = self.buffer.as_ref()?.peek()?;
        let serializer = self.json_serializer.as_ref()?;
        let result = serializer(&value);

        #[cfg(feature = "tracing")]
        tracing::debug!("Serialization result: {:?}", result.is_some());

        result
    }

    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn subscribe_json(&self) -> crate::DbResult<Box<dyn crate::buffer::JsonBufferReader + Send>> {
        use crate::DbError;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "subscribe_json called for type: {}",
            core::any::type_name::<T>()
        );

        // 1. Check if serialization is configured
        let serializer = self
            .json_serializer
            .clone()
            .ok_or_else(|| DbError::RuntimeError {
                message: format!(
                    "Record '{}' not configured with .with_remote_access(). \
                     Cannot subscribe to JSON stream.",
                    core::any::type_name::<T>()
                ),
            })?;

        // 2. Subscribe to the buffer (get Box<dyn BufferReader<T>>)
        let reader = self.subscribe()?;

        // 3. Wrap in JsonReaderAdapter
        let json_reader = JsonReaderAdapter {
            inner: reader,
            serializer,
        };

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Successfully created JSON subscription for type: {}",
            core::any::type_name::<T>()
        );

        Ok(Box::new(json_reader))
    }

    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn set_from_json(&self, json_value: serde_json::Value) -> crate::DbResult<()> {
        use crate::DbError;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "set_from_json called for type: {}",
            core::any::type_name::<T>()
        );

        // SAFETY CHECK 1: Enforce "No Producer Override" rule
        if self.has_producer_service() || self.has_transform() {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Rejected set_from_json for '{}': has active producer or transform",
                core::any::type_name::<T>()
            );

            return Err(DbError::PermissionDenied {
                operation: format!(
                    "Cannot set record '{}' - has active producer or transform. \
                     Use internal application logic instead. \
                     Remote access can only set configuration records without producers.",
                    core::any::type_name::<T>()
                ),
            });
        }

        // Check if deserialization is configured (need json_deserializer)
        let deserializer = self
            .json_deserializer
            .clone()
            .ok_or_else(|| DbError::RuntimeError {
                message: format!(
                    "Record '{}' not configured with .with_remote_access(). \
                     Cannot deserialize from JSON.",
                    core::any::type_name::<T>()
                ),
            })?;

        // Check if buffer exists
        if self.buffer.is_none() {
            return Err(DbError::RuntimeError {
                message: format!(
                    "Record '{}' has no buffer configured. \
                     Cannot produce value without buffer.",
                    core::any::type_name::<T>()
                ),
            });
        }

        // Deserialize JSON -> T
        let value: T = deserializer(&json_value).ok_or_else(|| DbError::RuntimeError {
            message: format!(
                "Failed to deserialize JSON to type '{}'. \
                 JSON structure does not match the expected schema.",
                core::any::type_name::<T>()
            ),
        })?;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Successfully deserialized JSON to type: {}",
            core::any::type_name::<T>()
        );

        // Push through the unified write path (design 031). This also marks
        // metadata as updated — previously skipped on this path.
        self.writer_handle().push(value);

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Successfully set value from JSON for record: {}",
            core::any::type_name::<T>()
        );

        Ok(())
    }

    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink] {
        &self.inbound_connectors
    }

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
