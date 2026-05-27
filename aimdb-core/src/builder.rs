//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;

use alloc::vec::Vec;
use hashbrown::HashMap;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(not(feature = "std"))]
use alloc::string::{String, ToString};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

use crate::extensions::Extensions;
use crate::graph::DependencyGraph;

/// Shorthand for a heap-pinned, `Send`, `'static` future — the unit of work
/// the `AimDbRunner` drives.
pub type BoxFuture = core::pin::Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Type-erased on_start function stored in `AimDbBuilder::start_fns`.
///
/// Defined once here so `on_start()` (which stores) and `build()` (which
/// downcasts) share the *exact same* type and a silent type mismatch cannot
/// cause a runtime panic. Single alias regardless of `std`/`no_std`.
type StartFnType<R> = Box<dyn FnOnce(Arc<R>) -> BoxFuture + Send>;

/// Type-erased per-record future collector stored in `AimDbBuilder::spawn_fns`.
///
/// At `build()` time each is invoked in topological order; the returned
/// `Vec<BoxFuture>` is appended to the runner's accumulator.
type SpawnFnType<R> =
    Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>, RecordId) -> DbResult<Vec<BoxFuture>> + Send>;
use crate::record_id::{RecordId, RecordKey, StringKey};
use crate::typed_api::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, RecordFutureCollector, TypedRecord};
use crate::{DbError, DbResult};

/// Type alias for outbound route tuples returned by `collect_outbound_routes`
///
/// Each tuple contains:
/// - `String` - Default topic/destination from the URL path
/// - `Box<dyn ConsumerTrait>` - Consumer for subscribing to record values
/// - `SerializerKind` - User-provided serializer for the record type (raw or context-aware)
/// - `Vec<(String, String)>` - Configuration options from the URL query
/// - `Option<TopicProviderFn>` - Optional dynamic topic provider
pub type OutboundRoute = (
    String,
    Box<dyn crate::connector::ConsumerTrait>,
    crate::connector::SerializerKind,
    Vec<(String, String)>,
    Option<crate::connector::TopicProviderFn>,
);

/// Marker type for untyped builder (before runtime is set)
pub struct NoRuntime;

/// Internal database state
///
/// Holds the registry of typed records with multiple index structures for
/// efficient access patterns:
///
/// - **`storages`**: Vec for O(1) hot-path access by RecordId
/// - **`by_key`**: HashMap for O(1) lookup by stable RecordKey
/// - **`by_type`**: HashMap for introspection (find all records of type T)
/// - **`types`**: Vec for runtime type validation during downcasts
/// - **`dependency_graph`**: DAG of record relationships (immutable after build)
pub struct AimDbInner {
    /// Record storage (hot path - indexed by RecordId)
    ///
    /// Order matches registration order. Immutable after build().
    storages: Vec<Box<dyn AnyRecord>>,

    /// Name → RecordId lookup (control plane)
    ///
    /// Used by remote access, CLI, MCP for O(1) name resolution.
    by_key: HashMap<StringKey, RecordId>,

    /// TypeId → RecordIds lookup (introspection)
    ///
    /// Enables "find all Temperature records" queries.
    by_type: HashMap<TypeId, Vec<RecordId>>,

    /// RecordId → TypeId lookup (type safety assertions)
    ///
    /// Used to validate downcasts at runtime.
    types: Vec<TypeId>,

    /// RecordId → StringKey lookup (reverse mapping)
    ///
    /// Used to get the key for a given record ID.
    keys: Vec<StringKey>,

    /// Dependency graph (immutable after build)
    ///
    /// Contains all record nodes, edges, and topological order.
    /// Used for introspection, spawn ordering, and graph queries.
    dependency_graph: crate::graph::DependencyGraph,

    /// Generic extension storage (frozen after build)
    ///
    /// External crates (e.g. `aimdb-persistence`) store typed state here
    /// during builder configuration and retrieve it at query time via
    /// `AimDb::extensions()`.
    pub(crate) extensions: Extensions,
}

impl AimDbInner {
    /// Resolve RecordKey to RecordId (control plane - O(1) average)
    #[inline]
    pub fn resolve<K: RecordKey>(&self, key: &K) -> Option<RecordId> {
        self.by_key.get(key.as_str()).copied()
    }

    /// Resolve string to RecordId (convenience for remote access)
    ///
    /// O(1) average thanks to `Borrow<str>` implementation on `RecordKey`.
    #[inline]
    pub fn resolve_str(&self, name: &str) -> Option<RecordId> {
        self.by_key.get(name).copied()
    }

    /// Get storage by RecordId (hot path - O(1))
    #[inline]
    pub fn storage(&self, id: RecordId) -> Option<&dyn AnyRecord> {
        self.storages.get(id.index()).map(|b| b.as_ref())
    }

    /// Get the StringKey for a given RecordId
    #[inline]
    pub fn key_for(&self, id: RecordId) -> Option<&StringKey> {
        self.keys.get(id.index())
    }

    /// Get all RecordIds for a type (introspection)
    pub fn records_of_type<T: 'static>(&self) -> &[RecordId] {
        self.by_type
            .get(&TypeId::of::<T>())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get the number of registered records
    #[inline]
    pub fn record_count(&self) -> usize {
        self.storages.len()
    }

    /// Get the dependency graph (immutable reference)
    ///
    /// The graph is constructed once during `build()` and never changes.
    /// Contains all record nodes, edges, and topological ordering.
    #[inline]
    pub fn dependency_graph(&self) -> &crate::graph::DependencyGraph {
        &self.dependency_graph
    }

    /// Helper to get a typed record by RecordKey
    ///
    /// This encapsulates the common pattern of:
    /// 1. Resolving key to RecordId
    /// 2. Validating TypeId matches
    /// 3. Downcasting to the typed record
    pub fn get_typed_record_by_key<T, R>(
        &self,
        key: impl AsRef<str>,
    ) -> DbResult<&TypedRecord<T, R>>
    where
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        let key_str = key.as_ref();

        // Resolve key to RecordId
        let id = self.resolve_str(key_str).ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RecordKeyNotFound {
                    key: key_str.to_string(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RecordKeyNotFound { _key: () }
            }
        })?;

        self.get_typed_record_by_id::<T, R>(id)
    }

    /// Helper to get a typed record by RecordId with type validation
    pub fn get_typed_record_by_id<T, R>(&self, id: RecordId) -> DbResult<&TypedRecord<T, R>>
    where
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        use crate::typed_record::AnyRecordExt;

        // Validate RecordId is in bounds
        if id.index() >= self.storages.len() {
            return Err(DbError::InvalidRecordId { id: id.raw() });
        }

        // Validate TypeId matches
        let expected = TypeId::of::<T>();
        let actual = self.types[id.index()];
        if expected != actual {
            #[cfg(feature = "std")]
            return Err(DbError::TypeMismatch {
                record_id: id.raw(),
                expected_type: core::any::type_name::<T>().to_string(),
            });
            #[cfg(not(feature = "std"))]
            return Err(DbError::TypeMismatch {
                record_id: id.raw(),
                _expected_type: (),
            });
        }

        // Safe to downcast (type validated above)
        let record = &self.storages[id.index()];

        #[cfg(feature = "std")]
        let typed_record = record.as_typed::<T, R>().ok_or(DbError::InvalidOperation {
            operation: "get_typed_record_by_id".to_string(),
            reason: "type mismatch during downcast".to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let typed_record = record.as_typed::<T, R>().ok_or(DbError::InvalidOperation {
            _operation: (),
            _reason: (),
        })?;

        Ok(typed_record)
    }

    /// Collects metadata for all registered records (std only)
    ///
    /// Returns a vector of `RecordMetadata` for remote access introspection.
    /// Available only when the `std` feature is enabled.
    #[cfg(feature = "std")]
    pub fn list_records(&self) -> Vec<crate::remote::RecordMetadata> {
        self.storages
            .iter()
            .enumerate()
            .map(|(i, record)| {
                let id = RecordId::new(i as u32);
                let type_id = self.types[i];
                let key = self.keys[i];
                record.collect_metadata(type_id, key, id)
            })
            .collect()
    }

    /// Try to get record's latest value as JSON by record key (std only)
    ///
    /// O(1) lookup using the key-based index.
    ///
    /// # Arguments
    /// * `record_key` - The record key (e.g., "sensors.temperature")
    ///
    /// # Returns
    /// `Some(JsonValue)` with the current record value, or `None`
    #[cfg(feature = "std")]
    pub fn try_latest_as_json(&self, record_key: &str) -> Option<serde_json::Value> {
        let id = self.resolve_str(record_key)?;
        self.storages.get(id.index())?.latest_json()
    }

    /// Sets a record value from JSON (remote access API)
    ///
    /// Deserializes the JSON value and writes it to the record's buffer.
    ///
    /// **SAFETY:** Enforces the "No Producer Override" rule:
    /// - Only works for records with `producer_count == 0`
    /// - Returns error if the record has active producers
    ///
    /// # Arguments
    /// * `record_key` - The record key (e.g., "config.app")
    /// * `json_value` - JSON representation of the value
    ///
    /// # Returns
    /// - `Ok(())` - Successfully set the value
    /// - `Err(DbError)` - If record not found, has producers, or deserialization fails
    #[cfg(feature = "std")]
    pub fn set_record_from_json(
        &self,
        record_key: &str,
        json_value: serde_json::Value,
    ) -> DbResult<()> {
        let id = self
            .resolve_str(record_key)
            .ok_or_else(|| DbError::RecordKeyNotFound {
                key: record_key.to_string(),
            })?;

        self.storages[id.index()].set_from_json(json_value)
    }
}

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
/// Use `.runtime()` to set the runtime and transition to a typed builder.
pub struct AimDbBuilder<R = NoRuntime> {
    /// Registered records with their keys (order matters for RecordId assignment)
    records: Vec<(StringKey, TypeId, Box<dyn AnyRecord>)>,

    /// Runtime adapter
    runtime: Option<Arc<R>>,

    /// Connector builders that will be invoked during build()
    connector_builders: Vec<Box<dyn crate::connector::ConnectorBuilder<R>>>,

    /// Spawn functions with their keys
    spawn_fns: Vec<(StringKey, Box<dyn core::any::Any + Send>)>,

    /// Startup tasks registered via on_start() — spawned after build() completes.
    /// Stored type-erased (Box<Box<dyn FnOnce(Arc<R>) -> BoxFuture<…>>>) to allow
    /// the field to exist on the unparameterised NoRuntime builder too.
    start_fns: Vec<Box<dyn core::any::Any + Send>>,

    /// Generic extension storage for external crates (e.g., persistence, metrics).
    /// Moved into AimDbInner during build() so it can be read on the live AimDb handle.
    extensions: Extensions,

    /// Remote access configuration (std only)
    #[cfg(feature = "std")]
    remote_config: Option<crate::remote::AimxConfig>,

    /// PhantomData to track the runtime type parameter
    _phantom: PhantomData<R>,
}

impl AimDbBuilder<NoRuntime> {
    /// Creates a new database builder without a runtime
    ///
    /// Call `.runtime()` to set the runtime adapter.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            runtime: None,
            connector_builders: Vec::new(),
            spawn_fns: Vec::new(),
            start_fns: Vec::new(),
            extensions: Extensions::new(),
            #[cfg(feature = "std")]
            remote_config: None,
            _phantom: PhantomData,
        }
    }

    /// Sets the runtime adapter
    ///
    /// This transitions the builder from untyped to typed with concrete runtime `R`.
    ///
    /// # Type Safety Note
    ///
    /// The `connector_builders` field is intentionally reset to `Vec::new()` during this
    /// transition because connectors are parameterized by the runtime type:
    ///
    /// - Before: `Vec<Box<dyn ConnectorBuilder<NoRuntime>>>`
    /// - After: `Vec<Box<dyn ConnectorBuilder<R>>>`
    ///
    /// These types are incompatible and cannot be transferred. However, this is not a bug
    /// because `.with_connector()` is only available AFTER calling `.runtime()` (it's defined
    /// in the `impl<R> where R: RuntimeAdapter` block, not in `impl AimDbBuilder<NoRuntime>`).
    ///
    /// This means the type system **enforces** the correct call order:
    /// ```rust,ignore
    /// AimDbBuilder::new()
    ///     .runtime(runtime)           // ← Must be called first
    ///     .with_connector(connector)  // ← Now available
    /// ```
    ///
    /// The `records` and `remote_config` are preserved across the transition since they
    /// are not parameterized by the runtime type.
    pub fn runtime<R>(self, rt: Arc<R>) -> AimDbBuilder<R>
    where
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        AimDbBuilder {
            records: self.records,
            runtime: Some(rt),
            connector_builders: Vec::new(),
            spawn_fns: Vec::new(),
            start_fns: self.start_fns,
            extensions: self.extensions,
            #[cfg(feature = "std")]
            remote_config: self.remote_config,
            _phantom: PhantomData,
        }
    }
}

impl<R> AimDbBuilder<R>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    /// Returns a shared reference to the extension storage.
    ///
    /// External crates use this to retrieve state stored via `extensions_mut()`.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Returns a mutable reference to the extension storage.
    ///
    /// Use this inside `AimDbBuilderPersistExt` (or similar) to store typed state
    /// that `.persist()` and `AimDbQueryExt` will later retrieve.
    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }

    /// Registers a task to be spawned after `build()` completes.
    ///
    /// The closure receives an `Arc<R>` (the runtime adapter) and must return a
    /// future that runs for as long as needed (e.g. an infinite cleanup loop).
    /// Tasks are spawned in registration order, after all record tasks and
    /// connectors have been started.
    ///
    /// # Example
    /// ```rust,ignore
    /// builder.on_start(|runtime| async move {
    ///     loop {
    ///         do_cleanup().await;
    ///         runtime.sleep(Duration::from_secs(3600)).await;
    ///     }
    /// });
    /// ```
    pub fn on_start<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(Arc<R>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        // Type-erase so the field can be shared with the `NoRuntime` builder struct.
        // Uses the module-level `StartFnType<R>` alias — must stay in sync with
        // the downcast in `build()`.
        let boxed: StartFnType<R> = Box::new(move |runtime| Box::pin(f(runtime)));
        self.start_fns.push(Box::new(boxed));
        self
    }

    /// Registers a connector builder that will be invoked during `build()`
    ///
    /// The connector builder will be called after the database is constructed,
    /// allowing it to collect routes and initialize the connector properly.
    ///
    /// # Arguments
    /// * `builder` - A connector builder that implements `ConnectorBuilder<R>`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_mqtt_connector::MqttConnector;
    ///
    /// let db = AimDbBuilder::new()
    ///     .runtime(runtime)
    ///     .with_connector(MqttConnector::new("mqtt://broker.local:1883"))
    ///     .configure::<Temperature>(|reg| {
    ///         reg.link_from("mqtt://commands/temp")...
    ///     })
    ///     .build().await?;
    /// ```
    pub fn with_connector(
        mut self,
        builder: impl crate::connector::ConnectorBuilder<R> + 'static,
    ) -> Self {
        self.connector_builders.push(Box::new(builder));
        self
    }

    /// Enables remote access via AimX protocol (std only)
    ///
    /// Configures the database to accept remote connections over a Unix domain socket,
    /// allowing external clients to introspect records, subscribe to updates, and
    /// (optionally) write data.
    ///
    /// The remote access supervisor will be spawned automatically during `build()`.
    ///
    /// # Arguments
    /// * `config` - Remote access configuration (socket path, security policy, etc.)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::remote::{AimxConfig, SecurityPolicy};
    ///
    /// let config = AimxConfig::new("/tmp/aimdb.sock")
    ///     .with_security(SecurityPolicy::read_only());
    ///
    /// let db = AimDbBuilder::new()
    ///     .runtime(runtime)
    ///     .with_remote_access(config)
    ///     .build()?;
    /// ```
    #[cfg(feature = "std")]
    pub fn with_remote_access(mut self, config: crate::remote::AimxConfig) -> Self {
        self.remote_config = Some(config);
        self
    }

    /// Configures a record type manually with a unique key
    ///
    /// The key uniquely identifies this record instance. Multiple records of the same
    /// type can exist with different keys (e.g., "sensor.temperature.room1" and
    /// "sensor.temperature.room2").
    ///
    /// # Arguments
    /// * `key` - A unique identifier for this record. Can be a string literal, `StringKey`,
    ///   or any type implementing `RecordKey` (including user-defined enum keys).
    /// * `f` - Configuration closure
    ///
    /// # Example
    /// ```rust,ignore
    /// // Using string literal
    /// builder.configure::<Temperature>("sensor.temp.room1", |reg| { ... });
    ///
    /// // Using compile-time safe enum key
    /// builder.configure::<Temperature>(SensorKey::TempRoom1, |reg| { ... });
    /// ```
    pub fn configure<T>(
        &mut self,
        key: impl RecordKey,
        f: impl for<'a> FnOnce(&'a mut RecordRegistrar<'a, T, R>),
    ) -> &mut Self
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Convert any RecordKey to StringKey for internal storage
        let record_key: StringKey = StringKey::from_dynamic(key.as_str());
        let type_id = TypeId::of::<T>();

        // Find existing record with this key, or create new one
        let record_index = self.records.iter().position(|(k, _, _)| k == &record_key);

        let (rec, is_new_record) = match record_index {
            Some(idx) => {
                // Use existing record
                let (_, existing_type, record) = &mut self.records[idx];
                assert!(
                    *existing_type == type_id,
                    "StringKey '{}' already registered with different type",
                    record_key.as_str()
                );
                (
                    record
                        .as_typed_mut::<T, R>()
                        .expect("type mismatch in record registry"),
                    false,
                )
            }
            None => {
                // Create new record
                self.records
                    .push((record_key, type_id, Box::new(TypedRecord::<T, R>::new())));
                let (_, _, record) = self.records.last_mut().unwrap();
                (
                    record
                        .as_typed_mut::<T, R>()
                        .expect("type mismatch in record registry"),
                    true,
                )
            }
        };

        let mut reg = RecordRegistrar {
            rec,
            connector_builders: &self.connector_builders,
            record_key: record_key.as_str().to_string(),
            extensions: &self.extensions,
            last_stage: None,
        };
        f(&mut reg);

        // Only store spawn function for new records to avoid duplicates
        if is_new_record {
            let spawn_key = record_key;

            let spawn_fn: SpawnFnType<R> =
                Box::new(move |runtime: &Arc<R>, db: &Arc<AimDb<R>>, id: RecordId| {
                    let typed_record = db.inner().get_typed_record_by_id::<T, R>(id)?;
                    // Resolve the record's key for key-based Producer/Consumer construction.
                    let key = db
                        .inner()
                        .key_for(id)
                        .map(|k| k.as_str().to_string())
                        .unwrap_or_else(|| alloc::format!("__record_{}", id.index()));
                    RecordFutureCollector::<T>::collect_all_futures(
                        typed_record,
                        runtime,
                        db,
                        key.as_str(),
                    )
                });

            // Store the spawn function (type-erased in Box<dyn Any>)
            self.spawn_fns.push((spawn_key, Box::new(spawn_fn)));
        }

        self
    }

    /// Registers a self-registering record type
    ///
    /// The record type must implement `RecordT<R>`.
    ///
    /// Uses the type name as the default key. For custom keys, use `configure()` directly.
    pub fn register_record<T>(&mut self, cfg: &T::Config) -> &mut Self
    where
        T: RecordT<R>,
    {
        // Default key is the full type name for backward compatibility
        let key = StringKey::new(core::any::type_name::<T>());
        self.configure::<T>(key, |reg| T::register(reg, cfg))
    }

    /// Registers a self-registering record type with a custom key
    ///
    /// The record type must implement `RecordT<R>`.
    pub fn register_record_with_key<T>(&mut self, key: impl RecordKey, cfg: &T::Config) -> &mut Self
    where
        T: RecordT<R>,
    {
        self.configure::<T>(key, |reg| T::register(reg, cfg))
    }

    /// Builds the database and drives every collected future to completion.
    ///
    /// Convenience wrapper for the common case: call `build()`, then immediately
    /// `runner.run().await`. The database handle is dropped on exit.
    ///
    /// For programmatic access to the handle (manual subscriptions, holding the
    /// `AimDb<R>` for other uses), prefer `build()` directly.
    ///
    /// # Returns
    /// `DbResult<()>` — Ok once the database starts; the call then blocks until
    /// every future the runner is driving has completed (typically forever).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[tokio::main]
    /// async fn main() -> DbResult<()> {
    ///     AimDbBuilder::new()
    ///         .runtime(Arc::new(TokioAdapter::new()?))
    ///         .configure::<MyData>(|reg| {
    ///             reg.with_buffer(BufferCfg::SpmcRing { capacity: 100 })
    ///                .with_source(my_producer)
    ///                .with_tap(my_consumer);
    ///         })
    ///         .run().await  // Runs forever
    /// }
    /// ```
    pub async fn run(self) -> DbResult<()>
    where
        R: crate::RuntimeForProfiling,
    {
        #[cfg(feature = "tracing")]
        tracing::info!("Building database and spawning background tasks...");

        let (_db, runner) = self.build().await?;

        #[cfg(feature = "tracing")]
        tracing::info!("Database running, runner driving collected futures.");

        runner.run().await;

        Ok(())
    }

    /// Builds the database and returns a `(handle, runner)` pair.
    ///
    /// `build()` collects every future the database needs to drive (producer
    /// services, consumer taps, transforms with fan-in forwarders, connectors,
    /// remote-access supervisor, `on_start` tasks) into the [`AimDbRunner`]
    /// returned alongside the clone-able [`AimDb<R>`] handle. **No background
    /// work runs until `runner.run().await` is awaited.**
    ///
    /// Use this when you need programmatic access to the handle for manual
    /// subscriptions or production. For typical services, use `.run().await`
    /// which calls `build()` and `runner.run()` for you.
    ///
    /// **Connector Setup:** Connectors are constructed during `build()` from
    /// the `ConnectorBuilder`s previously registered via `.with_connector()`.
    /// Their driving futures are appended to the runner.
    ///
    /// # Returns
    /// `DbResult<(AimDb<R>, AimDbRunner)>` — the handle (cloneable) and the
    /// non-`Clone` runner that owns the collected futures.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (db, runner) = AimDbBuilder::new()
    ///     .runtime(runtime)
    ///     .configure::<Temp>("temp", |reg| { /* … */ })
    ///     .with_connector(mqtt_builder)
    ///     .build().await?;
    ///
    /// let handle = db.clone();              // clone freely before runner.run()
    /// runner.run().await;                   // drives everything to completion
    /// ```
    pub async fn build(self) -> DbResult<(AimDb<R>, AimDbRunner)>
    where
        R: crate::RuntimeForProfiling,
    {
        // Validate all records
        for (key, _, record) in &self.records {
            record.validate().map_err(|_msg| {
                #[cfg(feature = "std")]
                {
                    DbError::RuntimeError {
                        message: format!("Record '{}' validation failed: {}", key.as_str(), _msg),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    // Suppress unused warning for key in no_std
                    let _ = &key;
                    DbError::RuntimeError { _message: () }
                }
            })?;
        }

        // Ensure runtime is set
        let runtime = self.runtime.ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: "runtime not set (use .runtime())".into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Build the new index structures
        let record_count = self.records.len();
        let mut storages: Vec<Box<dyn AnyRecord>> = Vec::with_capacity(record_count);
        let mut by_key: HashMap<StringKey, RecordId> = HashMap::with_capacity(record_count);
        let mut by_type: HashMap<TypeId, Vec<RecordId>> = HashMap::new();
        let mut types: Vec<TypeId> = Vec::with_capacity(record_count);
        let mut keys: Vec<StringKey> = Vec::with_capacity(record_count);

        for (i, (key, type_id, record)) in self.records.into_iter().enumerate() {
            let id = RecordId::new(i as u32);

            // Check for duplicate keys (should not happen if configure() is used correctly)
            if by_key.contains_key(&key) {
                #[cfg(feature = "std")]
                return Err(DbError::DuplicateRecordKey {
                    key: key.as_str().to_string(),
                });
                #[cfg(not(feature = "std"))]
                return Err(DbError::DuplicateRecordKey { _key: () });
            }

            // Build index structures
            storages.push(record);
            by_key.insert(key, id);
            by_type.entry(type_id).or_default().push(id);
            types.push(type_id);
            keys.push(key);
        }

        // Build dependency graph from record information
        // Collect RecordGraphInfo for each record
        let record_infos: Vec<crate::graph::RecordGraphInfo> = storages
            .iter()
            .enumerate()
            .map(|(idx, record)| {
                let key = keys[idx].as_str().to_string();
                let origin = record.record_origin();

                // Get buffer type and capacity from the record
                let (buffer_type, buffer_capacity) = record.buffer_info();

                crate::graph::RecordGraphInfo {
                    key,
                    origin,
                    buffer_type,
                    buffer_capacity,
                    tap_count: record.consumer_count(),
                    has_outbound_link: record.outbound_connector_count() > 0,
                }
            })
            .collect();

        // Build and validate the dependency graph
        let dependency_graph = DependencyGraph::build_and_validate(&record_infos)?;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Dependency graph built successfully ({} nodes, {} edges, topo order: {:?})",
            dependency_graph.nodes.len(),
            dependency_graph.edges.len(),
            dependency_graph.topo_order
        );

        let inner = Arc::new(AimDbInner {
            storages,
            by_key,
            by_type,
            types,
            keys,
            dependency_graph,
            extensions: self.extensions,
        });

        #[cfg(feature = "profiling")]
        let profiling_clock = crate::profiling::make_clock(runtime.clone());

        let db = Arc::new(AimDb {
            inner: inner.clone(),
            runtime: runtime.clone(),
            #[cfg(feature = "profiling")]
            profiling_clock,
        });

        // Accumulator for every future the runner will drive.
        let mut futures_acc: Vec<BoxFuture> = Vec::new();

        #[cfg(feature = "tracing")]
        tracing::info!("Collecting futures for {} records", self.spawn_fns.len());

        // Build a lookup map from spawn_fns for topological ordering
        let mut spawn_fn_map: HashMap<StringKey, Box<dyn core::any::Any + Send>> =
            self.spawn_fns.into_iter().collect();

        // Execute collectors in topological order — transforms collect after their inputs.
        for key_str in inner.dependency_graph.topo_order() {
            let key = match inner.by_key.keys().find(|k| k.as_str() == key_str) {
                Some(k) => *k,
                None => continue,
            };

            let spawn_fn_any = match spawn_fn_map.remove(&key) {
                Some(f) => f,
                None => continue,
            };

            let id = inner.resolve(&key).ok_or({
                #[cfg(feature = "std")]
                {
                    DbError::RecordKeyNotFound {
                        key: key.as_str().to_string(),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
                    DbError::RecordKeyNotFound { _key: () }
                }
            })?;

            let spawn_fn = spawn_fn_any
                .downcast::<SpawnFnType<R>>()
                .expect("spawn function type mismatch");

            futures_acc.extend((*spawn_fn)(&runtime, &db, id)?);
        }

        #[cfg(feature = "tracing")]
        tracing::info!("Record future collection complete");

        // Collect the remote-access supervisor future, if configured (std only).
        #[cfg(feature = "std")]
        if let Some(remote_cfg) = self.remote_config {
            #[cfg(feature = "tracing")]
            tracing::info!(
                "Building remote access supervisor for socket: {}",
                remote_cfg.socket_path.display()
            );

            // Apply security policy to mark writable records
            let writable_keys = remote_cfg.security_policy.writable_records();
            for key_str in writable_keys {
                if let Some(id) = inner.resolve_str(&key_str) {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("Marking record '{}' as writable", key_str);

                    inner.storages[id.index()].set_writable_erased(true);
                }
            }

            let supervisor_future =
                crate::remote::supervisor::build_supervisor_future(db.clone(), remote_cfg)?;
            futures_acc.push(supervisor_future);

            #[cfg(feature = "tracing")]
            tracing::info!("Remote access supervisor future collected");
        }

        // Collect connector futures. After issue #88 connector builders return
        // a `Vec<BoxFuture>` instead of an `Arc<dyn Connector>` (which previously
        // was discarded anyway — see design doc 028 §"The dropped Arc<dyn Connector> object").
        for builder in self.connector_builders {
            #[cfg(feature = "tracing")]
            let scheme = builder.scheme().to_string();

            #[cfg(feature = "tracing")]
            tracing::debug!("Building connector for scheme: {}", scheme);

            let connector_futures = builder.build(&db).await?;
            futures_acc.extend(connector_futures);

            #[cfg(feature = "tracing")]
            tracing::info!("Connector '{}' contributed {} future(s)", scheme, "n");
        }

        // Collect on_start futures (registered by external crates like aimdb-persistence).
        if !self.start_fns.is_empty() {
            #[cfg(feature = "tracing")]
            tracing::debug!("Collecting {} on_start future(s)", self.start_fns.len());

            for (idx, start_fn_any) in self.start_fns.into_iter().enumerate() {
                let start_fn = start_fn_any
                    .downcast::<StartFnType<R>>()
                    .unwrap_or_else(|_| {
                        panic!("on_start fn[{idx}] type mismatch — this is a bug in aimdb-core")
                    });
                futures_acc.push((*start_fn)(runtime.clone()));
            }
        }

        let db_owned = (*db).clone();

        Ok((db_owned, AimDbRunner::new(futures_acc)))
    }
}

impl Default for AimDbBuilder<NoRuntime> {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer-consumer database
///
/// A database instance with type-safe record registration and cross-record
/// communication via the Emitter pattern. The type parameter `R` represents
/// the runtime adapter (e.g., TokioAdapter, EmbassyAdapter).
///
/// See `examples/` for usage.
///
/// # Examples
///
/// ```rust,ignore
/// use aimdb_tokio_adapter::TokioAdapter;
///
/// let runtime = Arc::new(TokioAdapter);
/// let db: AimDb<TokioAdapter> = AimDbBuilder::new()
///     .runtime(runtime)
///     .register_record::<Temperature>(&TemperatureConfig)
///     .build()?;
/// ```
pub struct AimDb<R: aimdb_executor::RuntimeAdapter + 'static> {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter with concrete type
    runtime: Arc<R>,

    /// Shared wall clock for stage profiling, built from the runtime at `build()` time.
    #[cfg(feature = "profiling")]
    profiling_clock: crate::profiling::Clock,
}

impl<R: aimdb_executor::RuntimeAdapter + 'static> Clone for AimDb<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
            #[cfg(feature = "profiling")]
            profiling_clock: self.profiling_clock.clone(),
        }
    }
}

// ============================================================================
// AimDbRunner — drives every future the builder collected
// ============================================================================

/// Non-`Clone` runner returned alongside [`AimDb<R>`] from
/// [`AimDbBuilder::build()`].
///
/// Owns the complete set of futures that drive the database (producer
/// services, consumer taps, transforms with fan-in forwarders, connectors,
/// remote-access supervisor, `on_start` tasks). `run()` consumes `self`,
/// drives every future cooperatively via a `FuturesUnordered`, and returns
/// only when **every** future has completed — typically never, since
/// producer/consumer loops run for the application's lifetime.
///
/// Cancellation is the application's responsibility — wrap `runner.run()`
/// in `tokio::select!` with a shutdown signal, or use a `CancellationToken`
/// inside each registered service.
pub struct AimDbRunner {
    futures: Vec<BoxFuture>,
}

impl AimDbRunner {
    pub(crate) fn new(futures: Vec<BoxFuture>) -> Self {
        Self { futures }
    }

    /// Returns the number of futures the runner will drive. Useful for tests
    /// and diagnostic logging.
    pub fn future_count(&self) -> usize {
        self.futures.len()
    }

    /// Drives every collected future to completion.
    ///
    /// Returns when all futures complete (normally: never, while producer and
    /// consumer loops are alive). To cancel, wrap this call in `tokio::select!`
    /// (or the analogous primitive on Embassy / WASM) with a shutdown signal —
    /// when the `select!` arm fires, this future is dropped and every
    /// `FuturesUnordered` entry is cancelled with it.
    pub async fn run(self) {
        use futures_util::stream::{FuturesUnordered, StreamExt};

        if self.futures.is_empty() {
            return;
        }

        let mut set: FuturesUnordered<BoxFuture> = self.futures.into_iter().collect();
        while set.next().await.is_some() {}
    }
}

impl<R: aimdb_executor::RuntimeAdapter + 'static> AimDb<R> {
    /// Internal accessor for the inner state
    ///
    /// Used by adapter crates and internal spawning logic.
    #[doc(hidden)]
    pub fn inner(&self) -> &Arc<AimDbInner> {
        &self.inner
    }

    /// Returns the extension storage frozen at `build()` time.
    ///
    /// External crates (e.g. `aimdb-persistence`) retrieve their typed state here
    /// to service query calls. The extensions are read-only on the live handle.
    ///
    /// # Example
    /// ```rust,ignore
    /// use aimdb_persistence::PersistenceState;
    /// let state = db.extensions().get::<PersistenceState>().unwrap();
    /// ```
    pub fn extensions(&self) -> &Extensions {
        &self.inner.extensions
    }

    /// Shared wall clock used by stage profiling (nanoseconds since an arbitrary epoch).
    #[cfg(feature = "profiling")]
    pub(crate) fn profiling_clock(&self) -> &crate::profiling::Clock {
        &self.profiling_clock
    }

    /// Builds a database with a closure-based builder pattern
    pub async fn build_with(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder<R>)) -> DbResult<()>
    where
        R: crate::RuntimeForProfiling,
    {
        let mut b = AimDbBuilder::new().runtime(rt);
        f(&mut b);
        b.run().await
    }

    /// Produces a value to a specific record by key.
    ///
    /// Uses O(1) key-based lookup to find the correct record. Sync + fallible —
    /// only the lookup can fail; the buffer push itself is infallible.
    ///
    /// For hot paths, prefer `db.producer::<T>(key)?` once and reuse the
    /// returned handle.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    /// * `value` - The value to produce
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// db.produce::<Temperature>("sensors.indoor", indoor_temp)?;
    /// db.produce::<Temperature>("sensors.outdoor", outdoor_temp)?;
    /// ```
    pub fn produce<T>(&self, key: impl AsRef<str>, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Single write path via WriteHandle (design 031). For hot paths,
        // prefer `db.producer::<T>(key)` once and reuse the returned handle.
        let typed_rec = self.inner.get_typed_record_by_key::<T, R>(key)?;
        typed_rec.writer_handle().push(value);
        Ok(())
    }

    /// Subscribes to a specific record by key
    ///
    /// Uses O(1) key-based lookup to find the correct record.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut reader = db.subscribe::<Temperature>("sensors.indoor")?;
    /// while let Ok(temp) = reader.recv().await {
    ///     println!("Indoor: {:.1}°C", temp.celsius);
    /// }
    /// ```
    pub fn subscribe<T>(
        &self,
        key: impl AsRef<str>,
    ) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        let typed_rec = self.inner.get_typed_record_by_key::<T, R>(key)?;
        typed_rec.subscribe()
    }

    /// Creates a type-safe producer for a specific record by key
    ///
    /// Returns a `Producer<T>` bound to a specific record key.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let indoor_producer = db.producer::<Temperature>("sensors.indoor");
    /// let outdoor_producer = db.producer::<Temperature>("sensors.outdoor");
    ///
    /// // Each producer writes to its own record
    /// indoor_producer.produce(indoor_temp);
    /// outdoor_producer.produce(outdoor_temp);
    /// ```
    pub fn producer<T>(
        &self,
        key: impl Into<alloc::string::String>,
    ) -> DbResult<crate::typed_api::Producer<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Pre-resolve the typed record so the returned Producer holds a write
        // handle to the record's buffer/snapshot/metadata directly
        let key_str: alloc::string::String = key.into();
        let typed_rec = self.inner.get_typed_record_by_key::<T, R>(&key_str)?;
        Ok(crate::typed_api::Producer::new(typed_rec.writer_handle()))
    }

    /// Creates a type-safe consumer for a specific record by key
    ///
    /// Returns a `Consumer<T>` bound to a specific record key.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let indoor_consumer = db.consumer::<Temperature>("sensors.indoor");
    /// let outdoor_consumer = db.consumer::<Temperature>("sensors.outdoor");
    ///
    /// // Each consumer reads from its own record
    /// let mut rx = indoor_consumer.subscribe();
    /// ```
    pub fn consumer<T>(
        &self,
        key: impl Into<alloc::string::String>,
    ) -> DbResult<crate::typed_api::Consumer<T>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Pre-resolve the buffer Arc so the returned Consumer subscribes
        // through a direct virtual call (design 029). A `.consumer()` without
        // a configured buffer surfaces as `MissingConfiguration` here rather
        // than panicking later inside `subscribe()`.
        let key_str: alloc::string::String = key.into();
        let typed_rec = self.inner.get_typed_record_by_key::<T, R>(&key_str)?;
        let buffer = typed_rec.buffer_handle().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::MissingConfiguration {
                    parameter: alloc::format!("buffer for record '{}'", key_str),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::MissingConfiguration { _parameter: () }
            }
        })?;
        Ok(crate::typed_api::Consumer::new(buffer))
    }

    /// Resolve a record key to its RecordId
    ///
    /// Useful for checking if a record exists before operations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(id) = db.resolve_key("sensors.temperature") {
    ///     println!("Record exists with ID: {}", id);
    /// }
    /// ```
    pub fn resolve_key(&self, key: &str) -> Option<crate::record_id::RecordId> {
        self.inner.resolve_str(key)
    }

    /// Get all record IDs for a specific type
    ///
    /// Returns a slice of RecordIds for all records of type T.
    /// Useful for introspection when multiple records of the same type exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let temp_ids = db.records_of_type::<Temperature>();
    /// println!("Found {} temperature records", temp_ids.len());
    /// ```
    pub fn records_of_type<T: 'static>(&self) -> &[crate::record_id::RecordId] {
        self.inner.records_of_type::<T>()
    }

    /// Returns a reference to the runtime adapter
    ///
    /// Provides direct access to the concrete runtime type.
    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    /// Returns the runtime as a type-erased `Arc<dyn Any + Send + Sync>`
    ///
    /// Used by connectors to provide `RuntimeContext` to context-aware
    /// deserializers during inbound message routing.
    pub fn runtime_any(&self) -> Arc<dyn core::any::Any + Send + Sync> {
        self.runtime.clone()
    }

    /// Lists all registered records (std only)
    ///
    /// Returns metadata for all registered records, useful for remote access introspection.
    /// Available only when the `std` feature is enabled.
    ///
    /// # Example
    /// ```rust,ignore
    /// let records = db.list_records();
    /// for record in records {
    ///     println!("Record: {} ({})", record.name, record.type_id);
    /// }
    /// ```
    #[cfg(feature = "std")]
    pub fn list_records(&self) -> Vec<crate::remote::RecordMetadata> {
        self.inner.list_records()
    }

    /// Resets stage profiling counters for every record (feature `profiling`).
    #[cfg(feature = "profiling")]
    pub fn reset_stage_profiling(&self) {
        for record in &self.inner.storages {
            record.reset_profiling();
        }
    }

    /// Resets buffer introspection counters for every record (feature `metrics`).
    #[cfg(feature = "metrics")]
    pub fn reset_buffer_metrics(&self) {
        for record in &self.inner.storages {
            record.reset_buffer_metrics();
        }
    }

    /// Try to get record's latest value as JSON by name (std only)
    ///
    /// Convenience wrapper around `AimDbInner::try_latest_as_json()`.
    ///
    /// # Arguments
    /// * `record_name` - The full Rust type name (e.g., "server::Temperature")
    ///
    /// # Returns
    /// `Some(JsonValue)` with current value, or `None` if unavailable
    #[cfg(feature = "std")]
    pub fn try_latest_as_json(&self, record_name: &str) -> Option<serde_json::Value> {
        self.inner.try_latest_as_json(record_name)
    }

    /// Sets a record value from JSON (remote access API)
    ///
    /// Deserializes JSON and produces the value to the record's buffer.
    ///
    /// **SAFETY:** Enforces "No Producer Override" rule - only works for configuration
    /// records without active producers.
    ///
    /// # Arguments
    /// * `record_name` - Full Rust type name
    /// * `json_value` - JSON value to set
    ///
    /// # Returns
    /// `Ok(())` on success, error if record not found, has producers, or deserialization fails
    ///
    /// # Example (internal use)
    /// ```rust,ignore
    /// db.set_record_from_json("AppConfig", json!({"debug": true}))?;
    /// ```
    #[cfg(feature = "std")]
    pub fn set_record_from_json(
        &self,
        record_name: &str,
        json_value: serde_json::Value,
    ) -> DbResult<()> {
        self.inner.set_record_from_json(record_name, json_value)
    }

    /// Collects inbound connector routes for automatic router construction (std only)
    ///
    /// Iterates all records, filters their inbound_connectors by scheme,
    /// and returns routes with producer creation callbacks.
    ///
    /// # Arguments
    /// * `scheme` - URL scheme to filter by (e.g., "mqtt", "kafka")
    ///
    /// # Returns
    /// Vector of tuples: (topic, producer_trait, deserializer)
    ///
    /// The topic is resolved dynamically if a `TopicResolverFn` is configured,
    /// otherwise the static topic from the URL is used.
    ///
    /// # Example
    /// ```rust,ignore
    /// // In MqttConnector after db.build()
    /// let routes = db.collect_inbound_routes("mqtt");
    /// let router = RouterBuilder::from_routes(routes).build();
    /// connector.set_router(router).await?;
    /// ```
    pub fn collect_inbound_routes(
        &self,
        scheme: &str,
    ) -> Vec<(
        String,
        Box<dyn crate::connector::ProducerTrait>,
        crate::connector::DeserializerKind,
    )> {
        let mut routes = Vec::new();

        // Convert self to Arc<dyn Any> for producer factory
        let db_any: Arc<dyn core::any::Any + Send + Sync> = Arc::new(self.clone());

        for record in &self.inner.storages {
            let inbound_links = record.inbound_connectors();

            for link in inbound_links {
                // Filter by scheme
                if link.url.scheme() != scheme {
                    continue;
                }

                // Resolve topic: dynamic (from resolver) or static (from URL)
                let topic = link.resolve_topic();

                // Create producer using the stored factory
                if let Some(producer) = link.create_producer(db_any.clone()) {
                    routes.push((topic, producer, link.deserializer.clone()));
                }
            }
        }

        #[cfg(feature = "tracing")]
        if !routes.is_empty() {
            tracing::debug!(
                "Collected {} inbound routes for scheme '{}'",
                routes.len(),
                scheme
            );
        }

        routes
    }

    /// Collects outbound routes for a specific protocol scheme
    ///
    /// Mirrors `collect_inbound_routes()` for symmetry. Iterates all records,
    /// filters their outbound_connectors by scheme, and returns routes with
    /// consumer creation callbacks.
    ///
    /// This method is called by connectors during their `build()` phase to
    /// collect all configured outbound routes and spawn publisher tasks.
    ///
    /// # Arguments
    /// * `scheme` - URL scheme to filter by (e.g., "mqtt", "kafka")
    ///
    /// # Returns
    /// Vector of tuples: (destination, consumer_trait, serializer, config)
    ///
    /// The config Vec contains protocol-specific options (e.g., qos, retain).
    ///
    /// # Example
    /// ```rust,ignore
    /// // In MqttConnector::build()
    /// let routes = db.collect_outbound_routes("mqtt");
    /// for (topic, consumer, serializer, config) in routes {
    ///     connector.spawn_publisher(topic, consumer, serializer, config)?;
    /// }
    /// ```
    /// Collect `(topic, TypeId)` pairs for all outbound routes matching `scheme`.
    ///
    /// Complements [`collect_outbound_routes`](Self::collect_outbound_routes) when
    /// callers need to know the concrete record type behind each outbound topic
    /// (e.g. to resolve a schema name for discovery responses).
    ///
    /// The returned TypeId is the `TypeId::of::<T>()` for the record type `T`
    /// that was used in the corresponding `configure::<T>()` call.
    pub fn collect_outbound_topic_type_ids(&self, scheme: &str) -> Vec<(String, TypeId)> {
        let mut result = Vec::new();

        for (idx, record) in self.inner.storages.iter().enumerate() {
            let type_id = self.inner.types[idx];

            for link in record.outbound_connectors() {
                if link.url.scheme() != scheme {
                    continue;
                }
                result.push((link.url.resource_id(), type_id));
            }
        }

        result
    }

    pub fn collect_outbound_routes(&self, scheme: &str) -> Vec<OutboundRoute> {
        let mut routes = Vec::new();

        // Convert self to Arc<dyn Any> for consumer factory
        // This is necessary because the factory takes Arc<dyn Any> to avoid
        // needing to know the runtime type R at the factory definition site
        let db_any: Arc<dyn core::any::Any + Send + Sync> = Arc::new(self.clone());

        for record in &self.inner.storages {
            let outbound_links = record.outbound_connectors();

            for link in outbound_links {
                // Filter by scheme
                if link.url.scheme() != scheme {
                    continue;
                }

                let destination = link.url.resource_id();

                // Skip links without serializer
                let Some(serializer) = link.serializer.clone() else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Outbound link '{}' has no serializer, skipping", link.url);
                    continue;
                };

                // Create consumer using the stored factory
                if let Some(consumer) = link.create_consumer(db_any.clone()) {
                    routes.push((
                        destination,
                        consumer,
                        serializer,
                        link.config.clone(),
                        link.topic_provider.clone(),
                    ));
                }
            }
        }

        #[cfg(feature = "tracing")]
        if !routes.is_empty() {
            tracing::debug!(
                "Collected {} outbound routes for scheme '{}'",
                routes.len(),
                scheme
            );
        }

        routes
    }
}
