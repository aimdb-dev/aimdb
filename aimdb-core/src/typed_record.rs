//! Type-safe record storage using TypeId
//!
//! Provides type-safe record identification using Rust's `TypeId` for compile-time safety.
//!
//! # Feature Support
//!
//! **Both std and no_std**: Core API (`TypedRecord`, `latest()`, `RecordValue`, producer/consumer)
//!
//! **std only**: JSON serialization (`.with_serialization()`, `.as_json()`), remote access, metadata
//!
//! **no_std**: Use `record.latest()` for value access and `Deref` for fields. JSON requires std;
//! implement custom serialization for embedded protocols (CBOR, MessagePack, etc.).

use core::any::Any;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, string::String, sync::Arc, vec::Vec};

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
    /// Returns `Some(JsonValue)` if record was configured with `.with_serialization()`,
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
    /// JSON serializer function (from .with_serialization())
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
}

/// Metadata tracking for records (std only - used for remote access introspection)
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
struct RecordMetadataTracker {
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

    fn mark_updated(&self) {
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
type BoxFuture<'a, T> = core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;

/// Type alias for consumer service closure stored in TypedRecord
/// Each consumer receives a Consumer<T, R> handle for subscribing to the buffer
/// and a runtime context (Arc<dyn Any>) for accessing runtime capabilities
type ConsumerServiceFn<T, R> = Box<
    dyn FnOnce(crate::Consumer<T, R>, Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, ()> + Send,
>;

/// Type alias for producer service closure stored in TypedRecord
/// Takes (Producer<T, R>, RuntimeContext) and returns a Future
/// This will be auto-spawned during build()
type ProducerServiceFn<T, R> = Box<
    dyn FnOnce(crate::Producer<T, R>, Arc<dyn Any + Send + Sync>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
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

    /// Returns the number of registered connectors
    fn connector_count(&self) -> usize;

    /// Returns the connector URLs as strings
    #[cfg(feature = "std")]
    fn connector_urls(&self) -> Vec<String>;

    /// Gets the connector links
    ///
    /// Returns connector configuration list for spawning logic.
    fn connectors(&self) -> &[crate::connector::ConnectorLink];

    /// Returns the number of registered consumers (tap observers)
    fn consumer_count(&self) -> usize;

    /// Returns whether a producer service is registered
    fn has_producer_service(&self) -> bool;

    /// Sets the writable flag for this record (type-erased)
    ///
    /// Used internally by the builder to apply security policy to records.
    fn set_writable_erased(&self, writable: bool);

    /// Spawns outbound consumers for connector links (internal use)
    ///
    /// Called during build() after connectors are constructed. Creates consumer tasks
    /// that subscribe to the buffer and publish to external systems via connectors.
    ///
    /// Takes type-erased parameters to maintain dyn-compatibility.
    fn spawn_outbound_consumers(
        &self,
        runtime: &dyn core::any::Any,
        db: &dyn core::any::Any,
        connectors: &dyn core::any::Any,
    ) -> crate::DbResult<()>;

    /// Collects metadata for this record (std only)
    #[cfg(feature = "std")]
    fn collect_metadata(&self, type_id: core::any::TypeId) -> crate::remote::RecordMetadata;

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
    /// - Record not configured with `.with_serialization()`
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
    /// - Record not configured with `.with_serialization()`
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
    fn as_typed<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static>(
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
    fn as_typed_mut<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static>(
        &mut self,
    ) -> Option<&mut TypedRecord<T, R>>;
}

impl AnyRecordExt for Box<dyn AnyRecord> {
    fn as_typed<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static>(
        &self,
    ) -> Option<&TypedRecord<T, R>> {
        self.as_any().downcast_ref::<TypedRecord<T, R>>()
    }

    fn as_typed_mut<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static>(
        &mut self,
    ) -> Option<&mut TypedRecord<T, R>> {
        self.as_any_mut().downcast_mut::<TypedRecord<T, R>>()
    }
}

/// Helper for spawning tasks for a specific record type
///
/// Captures record type `T` via PhantomData to spawn tasks without making AnyRecord non-object-safe.
pub struct RecordSpawner<T> {
    _phantom: core::marker::PhantomData<T>,
}

impl<T> RecordSpawner<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Spawns all tasks (producer, consumers, and inbound connectors) for a record
    ///
    /// Downcasts type-erased AnyRecord to TypedRecord<T, R> and spawns tasks.
    ///
    /// # Arguments
    /// * `record` - The type-erased record to spawn tasks for
    /// * `runtime` - The runtime adapter for spawning tasks
    /// * `db` - The database instance
    pub fn spawn_all_tasks<R>(
        record: &dyn AnyRecord,
        runtime: &Arc<R>,
        db: &Arc<crate::builder::AimDb<R>>,
    ) -> crate::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
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

        // Spawn producer service if present
        if typed_record.has_producer_service() {
            typed_record.spawn_producer_service(runtime, db)?;
        }

        // Spawn consumer tasks if present
        if typed_record.consumer_count() > 0 {
            typed_record.spawn_consumer_tasks(runtime, db)?;
        }

        Ok(())
    }
}

/// Typed record storage with producer/consumer functions
///
/// Stores type-safe producer and consumer functions with optional buffering for async dispatch.
pub struct TypedRecord<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> {
    /// Optional producer service - a task that generates data
    /// This will be auto-spawned during build() if present
    /// Stored as FnOnce that takes (Producer<T, R>, RuntimeContext) and returns a Future
    /// Wrapped in Mutex for interior mutability (needed to take() during spawning)
    #[cfg(feature = "std")]
    producer_service: std::sync::Mutex<Option<ProducerServiceFn<T, R>>>,

    #[cfg(not(feature = "std"))]
    producer_service: spin::Mutex<Option<ProducerServiceFn<T, R>>>,

    /// List of consumer/tap tasks - wrapped in Mutex for Sync + taking out during spawn
    /// Each is spawned as an independent background task that subscribes to the buffer
    /// Using Mutex provides the Sync bound required by AnyRecord trait
    #[cfg(feature = "std")]
    consumers: std::sync::Mutex<Vec<ConsumerServiceFn<T, R>>>,

    #[cfg(not(feature = "std"))]
    consumers: spin::Mutex<alloc::vec::Vec<ConsumerServiceFn<T, R>>>,

    /// Optional buffer for async dispatch
    /// When present, produce() enqueues to buffer instead of direct call
    buffer: Option<Box<dyn DynBuffer<T>>>,

    /// Buffer configuration (cached for metadata, std only)
    #[cfg(feature = "std")]
    buffer_cfg: Option<crate::buffer::BufferCfg>,

    /// List of outbound connector links (AimDB → External)
    /// Each link represents a protocol connector (MQTT, Kafka, HTTP, etc.)
    connectors: Vec<crate::connector::ConnectorLink>,

    /// List of inbound connector links (External → AimDB)
    /// Each link spawns a background task that subscribes to an external source
    /// and produces values into this record's buffer
    /// Inbound connector links (External → AimDB)
    inbound_connectors: Vec<crate::connector::InboundConnectorLink>,

    /// Metadata tracking (std only - for remote access)
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,

    /// JSON serializer function (std only - for remote access)
    /// When set via .with_serialization(), automatically serializes values for record.get queries
    /// Stores the serialization logic where T: Serialize is known at call site
    #[cfg(feature = "std")]
    json_serializer: Option<JsonSerializer<T>>,

    /// JSON deserializer function (std only - for remote access)
    /// When set via .with_serialization(), automatically deserializes JSON for record.set operations
    /// Stores the deserialization logic where T: Deserialize is known at call site
    #[cfg(feature = "std")]
    json_deserializer: Option<JsonDeserializer<T>>,

    /// Latest value snapshot - for latest() API
    /// Cached atomically on every produce() call to support latest()
    /// This provides a buffer-agnostic way to query the latest value
    /// Available in both std and no_std environments
    #[cfg(feature = "std")]
    latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,

    #[cfg(not(feature = "std"))]
    latest_snapshot: Arc<spin::Mutex<Option<T>>>,
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> TypedRecord<T, R> {
    /// Creates a new empty typed record
    ///
    /// Call `.with_serialization()` to enable JSON (std only).
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "std")]
            producer_service: std::sync::Mutex::new(None),
            #[cfg(not(feature = "std"))]
            producer_service: spin::Mutex::new(None),
            #[cfg(feature = "std")]
            consumers: std::sync::Mutex::new(Vec::new()),
            #[cfg(not(feature = "std"))]
            consumers: spin::Mutex::new(alloc::vec::Vec::new()),
            buffer: None,
            #[cfg(feature = "std")]
            buffer_cfg: None,
            connectors: Vec::new(),
            inbound_connectors: Vec::new(),
            #[cfg(feature = "std")]
            metadata: RecordMetadataTracker::new::<T>(),
            #[cfg(feature = "std")]
            json_serializer: None,
            #[cfg(feature = "std")]
            json_deserializer: None,
            #[cfg(feature = "std")]
            latest_snapshot: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(not(feature = "std"))]
            latest_snapshot: Arc::new(spin::Mutex::new(None)),
        }
    }

    /// Sets the producer service for this record
    ///
    /// Long-running task that generates data via `producer.produce()`. Auto-spawned during `build()`.
    ///
    /// # Panics
    /// Panics if producer already set (one producer per record).
    pub fn set_producer_service<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::Producer<T, R>, Arc<dyn Any + Send + Sync>) -> Fut + Send + Sync + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Check if already set
        #[cfg(feature = "std")]
        let already_set = self.producer_service.lock().unwrap().is_some();
        #[cfg(not(feature = "std"))]
        let already_set = self.producer_service.lock().is_some();

        if already_set {
            panic!("This record type already has a producer service");
        }

        // Box the future-returning function
        let boxed_fn = Box::new(
            move |producer: crate::Producer<T, R>,
                  ctx: Arc<dyn Any + Send + Sync>|
                  -> BoxFuture<'static, ()> { Box::pin(f(producer, ctx)) },
        );

        // Store it in the mutex
        #[cfg(feature = "std")]
        {
            *self.producer_service.lock().unwrap() = Some(boxed_fn);
        }
        #[cfg(not(feature = "std"))]
        {
            *self.producer_service.lock() = Some(boxed_fn);
        }
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
    ///     let mut rx = consumer.subscribe().unwrap();
    ///     while let Ok(value) = rx.recv().await {
    ///         println!("Consumer: {:?}", value);
    ///     }
    /// });
    /// ```
    pub fn add_consumer<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(crate::Consumer<T, R>, Arc<dyn Any + Send + Sync>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Box the future to make it trait object compatible
        let boxed = Box::new(
            move |consumer: crate::Consumer<T, R>,
                  ctx_any: Arc<dyn Any + Send + Sync>|
                  -> BoxFuture<'static, ()> { Box::pin(f(consumer, ctx_any)) },
        );

        #[cfg(feature = "std")]
        {
            self.consumers.lock().unwrap().push(boxed);
        }

        #[cfg(not(feature = "std"))]
        {
            self.consumers.lock().push(boxed);
        }
    }

    /// Sets the buffer for this record
    ///
    /// When set, `produce()` enqueues values for async dispatch to consumers.
    pub fn set_buffer(&mut self, buffer: Box<dyn DynBuffer<T>>) {
        // Cache buffer configuration for metadata (std only)
        #[cfg(feature = "std")]
        {
            // Store a simplified version of the config for metadata
            // We can't call cfg() on the buffer, so we'll infer from the buffer type name
            self.buffer_cfg = None; // Will be set by the caller via set_buffer_cfg
        }
        self.buffer = Some(buffer);
    }

    /// Sets the buffer configuration (for metadata tracking, std only)
    #[cfg(feature = "std")]
    pub fn set_buffer_cfg(&mut self, cfg: crate::buffer::BufferCfg) {
        self.buffer_cfg = Some(cfg);
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

    /// Adds a connector link for external system integration
    ///
    /// Bridges records to external protocols (MQTT, Kafka, HTTP, etc.).
    /// Multiple connectors supported per record.
    pub fn add_connector(&mut self, link: crate::connector::ConnectorLink) {
        self.connectors.push(link);
    }

    /// Enables JSON serialization for remote access (std only)
    ///
    /// Enables `record.latest()?.as_json()` and remote access `record.get` protocol.
    /// Requires `std` feature and `T: serde::Serialize`.
    ///
    /// For no_std, use `record.latest()` for value access or custom serialization.
    #[cfg(feature = "std")]
    pub fn with_serialization(&mut self) -> &mut Self
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
            "with_serialization() called for record type: {}",
            core::any::type_name::<T>()
        );

        self
    }

    /// Enables JSON serialization (backward compatible, read-only)
    ///
    /// This version only adds serialization support (for `record.get` and `record.subscribe`).
    /// For write support via `record.set`, use `.with_serialization()` which requires
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

    /// Returns a reference to the registered connectors
    ///
    /// # Returns
    /// A slice of connector links
    pub fn connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.connectors
    }

    /// Returns the number of registered connectors
    ///
    /// # Returns
    /// The count of connectors
    pub fn connector_count(&self) -> usize {
        self.connectors.len()
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
    pub fn add_inbound_connector(&mut self, link: crate::connector::InboundConnectorLink) {
        self.inbound_connectors.push(link);
    }

    /// Returns the number of registered consumers
    ///
    /// # Returns
    /// The count of consumers
    pub fn consumer_count(&self) -> usize {
        #[cfg(feature = "std")]
        {
            self.consumers.lock().unwrap().len()
        }

        #[cfg(not(feature = "std"))]
        {
            self.consumers.lock().len()
        }
    }

    /// Spawns all registered consumer tasks
    ///
    /// Spawns `.tap()` consumers as background tasks. Called automatically by `Database::new()`.
    /// Consumes registered consumers (FnOnce), can only be called once.
    pub fn spawn_consumer_tasks(
        &self,
        runtime: &Arc<R>,
        db: &Arc<crate::AimDb<R>>,
    ) -> crate::DbResult<()>
    where
        R: aimdb_executor::Spawn,
        T: Sync,
    {
        use crate::DbError;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Spawning {} consumer tasks for record type {}",
            self.consumer_count(),
            core::any::type_name::<T>()
        );

        // Take all consumers from the Mutex
        let consumers = {
            #[cfg(feature = "std")]
            {
                core::mem::take(&mut *self.consumers.lock().unwrap())
            }

            #[cfg(not(feature = "std"))]
            {
                core::mem::take(&mut *self.consumers.lock())
            }
        };

        #[cfg(feature = "tracing")]
        let count = consumers.len();

        // Spawn each consumer as a background task
        for consumer_fn in consumers {
            // Create a Consumer<T> handle for this task
            let consumer = crate::typed_api::Consumer::new(db.clone());

            // Get runtime context as Arc<dyn Any> by cloning the Arc
            let runtime_any: Arc<dyn Any + Send + Sync> = runtime.clone();

            // Spawn the consumer task with runtime context
            let task_future = consumer_fn(consumer, runtime_any);
            runtime.spawn(task_future).map_err(DbError::from)?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Successfully spawned {} consumer tasks for record type {}",
            count,
            core::any::type_name::<T>()
        );

        Ok(())
    }

    /// Spawns consumer tasks from a type-erased runtime
    ///
    /// Helper for automatic spawning system via database's spawn_task method.
    pub fn spawn_producer_service(
        &self,
        runtime: &Arc<R>,
        db: &Arc<crate::AimDb<R>>,
    ) -> crate::DbResult<()>
    where
        R: aimdb_executor::Spawn,
        T: Sync,
    {
        use crate::DbError;

        // Take the producer service (can only spawn once)
        let service = {
            #[cfg(feature = "std")]
            {
                self.producer_service.lock().unwrap().take()
            }
            #[cfg(not(feature = "std"))]
            {
                self.producer_service.lock().take()
            }
        };

        if let Some(service_fn) = service {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Spawning producer service for record type {}",
                core::any::type_name::<T>()
            );

            // Create Producer<T> and pass the type-erased runtime context
            let producer = crate::typed_api::Producer::new(db.clone());
            let ctx: Arc<dyn core::any::Any + Send + Sync> = runtime.clone();

            // Call the service function to get the future
            let task_future = service_fn(producer, ctx);

            // Spawn the producer service task using the typed runtime
            runtime.spawn(task_future).map_err(DbError::from)?;

            #[cfg(feature = "tracing")]
            tracing::info!(
                "Successfully spawned producer service for record type {}",
                core::any::type_name::<T>()
            );
        }

        Ok(())
    }

    /// Produces a value by pushing to the buffer
    ///
    /// Enqueues value for consumer tasks and updates latest snapshot.
    pub async fn produce(&self, val: T) {
        // Cache snapshot for latest() API (both std and no_std)
        #[cfg(feature = "std")]
        {
            *self.latest_snapshot.lock().unwrap() = Some(val.clone());
        }

        #[cfg(not(feature = "std"))]
        {
            *self.latest_snapshot.lock() = Some(val.clone());
        }

        // Push to buffer - consumer tasks will receive it
        if let Some(buf) = &self.buffer {
            buf.push(val);

            // Update metadata timestamp (std only)
            #[cfg(feature = "std")]
            self.metadata.mark_updated();
        }
    }

    /// Returns whether a producer service is registered
    pub fn has_producer_service(&self) -> bool {
        #[cfg(feature = "std")]
        {
            self.producer_service.lock().unwrap().is_some()
        }
        #[cfg(not(feature = "std"))]
        {
            self.producer_service.lock().is_some()
        }
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
    /// **std only**: `.as_json()` (if `.with_serialization()` configured)
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
        #[cfg(feature = "std")]
        {
            let value = self.latest_snapshot.lock().unwrap().clone()?;
            Some(RecordValue::new(value, self.json_serializer.clone()))
        }

        #[cfg(not(feature = "std"))]
        {
            let value = self.latest_snapshot.lock().clone()?;
            Some(RecordValue::new(value, None))
        }
    }

    /// Creates a boxed ProducerTrait for this record type (std only)
    ///
    /// Returns a type-erased producer that implements ProducerTrait,
    /// allowing inbound connectors to produce values without knowing the concrete type.
    ///
    /// # Arguments
    /// * `db` - Database reference for creating the typed producer
    ///
    /// # Returns
    /// Box<dyn ProducerTrait> that can be used for routing
    #[cfg(feature = "std")]
    pub fn create_producer_trait(
        &self,
        db: Arc<crate::builder::AimDb<R>>,
    ) -> Box<dyn crate::connector::ProducerTrait> {
        Box::new(crate::typed_api::Producer::<T, R>::new(db))
    }
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> Default
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
impl<T: Send + Sync + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> AnyRecord
    for TypedRecord<T, R>
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

    fn connector_count(&self) -> usize {
        self.connectors.len()
    }

    #[cfg(feature = "std")]
    fn connector_urls(&self) -> Vec<String> {
        self.connectors
            .iter()
            .map(|link| format!("{}", link.url))
            .collect()
    }

    fn connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.connectors
    }

    fn consumer_count(&self) -> usize {
        TypedRecord::consumer_count(self)
    }

    fn has_producer_service(&self) -> bool {
        TypedRecord::has_producer_service(self)
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

    fn spawn_outbound_consumers(
        &self,
        runtime_any: &dyn core::any::Any,
        db_any: &dyn core::any::Any,
        connectors_any: &dyn core::any::Any,
    ) -> crate::DbResult<()> {
        #[cfg(not(feature = "std"))]
        use alloc::collections::BTreeMap;
        #[cfg(feature = "std")]
        use std::collections::BTreeMap;

        // Downcast parameters
        let runtime = runtime_any.downcast_ref::<Arc<R>>().ok_or({
            #[cfg(feature = "std")]
            {
                crate::DbError::Internal {
                    code: 0x7001,
                    message: "Failed to downcast runtime in spawn_outbound_consumers".into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                crate::DbError::Internal {
                    code: 0x7001,
                    _message: (),
                }
            }
        })?;

        let db = db_any
            .downcast_ref::<Arc<crate::builder::AimDb<R>>>()
            .ok_or({
                #[cfg(feature = "std")]
                {
                    crate::DbError::Internal {
                        code: 0x7001,
                        message: "Failed to downcast db in spawn_outbound_consumers".into(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    crate::DbError::Internal {
                        code: 0x7001,
                        _message: (),
                    }
                }
            })?;

        let connectors = connectors_any
            .downcast_ref::<BTreeMap<String, Arc<dyn crate::transport::Connector>>>()
            .ok_or({
                #[cfg(feature = "std")]
                {
                    crate::DbError::Internal {
                        code: 0x7001,
                        message: "Failed to downcast connectors in spawn_outbound_consumers".into(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    crate::DbError::Internal {
                        code: 0x7001,
                        _message: (),
                    }
                }
            })?;

        // Get the connector links for this record
        let links = self.connectors();

        for link in links {
            let scheme = link.url.scheme();

            // Get the connector for this scheme
            let Some(connector) = connectors.get(scheme) else {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "No connector found for scheme '{}' (link: {})",
                    scheme,
                    link.url
                );
                continue;
            };

            let Some(serializer) = &link.serializer else {
                #[cfg(feature = "tracing")]
                tracing::warn!("No serializer for outbound link: {}", link.url);
                continue;
            };

            // Create consumer closure that publishes to the connector
            let connector_clone = connector.clone();
            let url_string = format!("{}", link.url);
            let config = link.config.clone();
            let serializer_clone = serializer.clone();
            let db_clone = db.clone();

            runtime.spawn(async move {
                // Get consumer for this record type - use pub(crate) constructor
                let consumer = crate::typed_api::Consumer::<T, R>::new(db_clone.clone());

                let Ok(mut reader) = consumer.subscribe() else {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to subscribe to buffer for connector {}", url_string);
                    return;
                };

                while let Ok(value) = reader.recv().await {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "Connector triggered for {} with type {}",
                        url_string,
                        core::any::type_name::<T>()
                    );

                    // Serialize the value using type-erased serializer
                    let bytes = match serializer_clone(&value as &dyn core::any::Any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!("Failed to serialize for {}: {:?}", url_string, _e);
                            continue;
                        }
                    };

                    // Publish via connector
                    let publish_result = {
                        use crate::connector::ConnectorUrl;
                        let url = ConnectorUrl::parse(&url_string).expect("Invalid URL");
                        let destination = url.resource_id();

                        let mut connector_config = crate::transport::ConnectorConfig {
                            qos: 0,
                            retain: false,
                            timeout_ms: Some(5000),
                            protocol_options: Vec::new(),
                        };

                        // Parse config
                        for (key, value) in &config {
                            match key.as_str() {
                                "qos" => {
                                    if let Ok(qos) = value.parse::<u8>() {
                                        connector_config.qos = qos;
                                    }
                                }
                                "retain" => {
                                    if let Ok(retain) = value.parse::<bool>() {
                                        connector_config.retain = retain;
                                    }
                                }
                                "timeout_ms" => {
                                    if let Ok(timeout) = value.parse::<u32>() {
                                        connector_config.timeout_ms = Some(timeout);
                                    }
                                }
                                _ => {}
                            }
                        }

                        connector_clone
                            .publish(&destination, &connector_config, &bytes)
                            .await
                    };

                    if let Err(_e) = publish_result {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Failed to publish to {}: {:?}", url_string, _e);
                    }
                }
            })?;
        }

        Ok(())
    }

    #[cfg(feature = "std")]
    fn collect_metadata(&self, type_id: core::any::TypeId) -> crate::remote::RecordMetadata {
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

        crate::remote::RecordMetadata::new(
            type_id,
            self.metadata.name.clone(),
            buffer_type,
            buffer_capacity,
            if self.has_producer_service() { 1 } else { 0 },
            self.consumer_count(),
            self.metadata
                .writable
                .load(std::sync::atomic::Ordering::SeqCst),
            RecordMetadataTracker::format_timestamp(self.metadata.created_at),
            self.connector_count(),
        )
        .with_last_update_opt(last_update)
    }

    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn latest_json(&self) -> Option<serde_json::Value> {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "latest_json called for type: {}",
            core::any::type_name::<T>()
        );

        // Delegate to latest() which returns RecordValue<T> with serializer attached
        let result = self.latest().and_then(|v| v.as_json());

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
                    "Record '{}' not configured with .with_serialization(). \
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
        if self.has_producer_service() {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Rejected set_from_json for '{}': has active producer (producer_count=1)",
                core::any::type_name::<T>()
            );

            return Err(DbError::PermissionDenied {
                operation: format!(
                    "Cannot set record '{}' - has active producer. \
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
                    "Record '{}' not configured with .with_serialization(). \
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

        // Check if buffer exists before trying to produce
        if self.buffer.is_none() {
            return Err(DbError::RuntimeError {
                message: format!(
                    "Record '{}' has no buffer configured. \
                     Cannot produce value without buffer.",
                    core::any::type_name::<T>()
                ),
            });
        }

        // Use the existing produce() method which handles:
        // 1. Updating latest_snapshot
        // 2. Pushing to buffer
        // 3. Updating metadata timestamp (std only)
        // This is a synchronous wrapper around the async produce()
        {
            #[cfg(feature = "std")]
            {
                *self.latest_snapshot.lock().unwrap() = Some(value.clone());
            }

            #[cfg(not(feature = "std"))]
            {
                *self.latest_snapshot.lock() = Some(value.clone());
            }

            if let Some(buf) = &self.buffer {
                buf.push(value);

                #[cfg(feature = "std")]
                self.metadata.mark_updated();
            }
        }

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
}
