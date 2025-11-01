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
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc, vec::Vec};

use crate::buffer::DynBuffer;

/// Type alias for JSON serializer function (std only)
#[cfg(feature = "std")]
type JsonSerializer<T> = Arc<dyn Fn(&T) -> Option<serde_json::Value> + Send + Sync>;

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
    /// Whether this record allows writes via remote access
    writable: bool,
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
            writable: false,
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

    fn set_writable(&mut self, writable: bool) {
        self.writable = writable;
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
/// Note: This trait requires both `Send` and `Sync` because:
/// - Records are stored in Arc and shared across threads
/// - Emitter needs to be Send+Sync to work in async contexts
/// - The FnOnce consumers are moved out during spawning, so they don't affect Sync
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

    /// Collects metadata for this record (std only)
    #[cfg(feature = "std")]
    fn collect_metadata(&self, type_id: core::any::TypeId) -> crate::remote::RecordMetadata;

    /// Internal: Returns JSON for type-erased remote access (std only)
    ///
    /// Used internally by remote access protocol. **Users should use `record.latest()?.as_json()`.**
    #[doc(hidden)]
    #[cfg(feature = "std")]
    fn latest_json(&self) -> Option<serde_json::Value>;
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
    /// Spawns all tasks (producer and consumers) for a record
    ///
    /// Downcasts type-erased AnyRecord to TypedRecord<T, R> and spawns tasks.
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

    /// List of connector links for external system integration
    /// Each link represents a protocol connector (MQTT, Kafka, HTTP, etc.)
    connectors: Vec<crate::connector::ConnectorLink>,

    /// Metadata tracking (std only - for remote access)
    #[cfg(feature = "std")]
    metadata: RecordMetadataTracker,

    /// JSON serializer function (std only - for remote access)
    /// When set via .with_serialization(), automatically serializes values for record.get queries
    /// Stores the serialization logic where T: Serialize is known at call site
    #[cfg(feature = "std")]
    json_serializer: Option<JsonSerializer<T>>,

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
            #[cfg(feature = "std")]
            metadata: RecordMetadataTracker::new::<T>(),
            #[cfg(feature = "std")]
            json_serializer: None,
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
        T: serde::Serialize,
    {
        // Store serialization function where T: Serialize is known
        self.json_serializer = Some(std::sync::Arc::new(|val: &T| {
            serde_json::to_value(val).ok()
        }));

        #[cfg(feature = "tracing")]
        tracing::info!(
            "with_serialization() called for record type: {}",
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
    pub fn set_writable(&mut self, writable: bool) {
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
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> Default
    for TypedRecord<T, R>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + 'static + Debug + Clone, R: aimdb_executor::Spawn + 'static> AnyRecord
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
            self.metadata.writable,
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
}
