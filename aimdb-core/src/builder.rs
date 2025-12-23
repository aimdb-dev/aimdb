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

#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::string::{String, ToString};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

use crate::record_id::{RecordId, RecordKey, StringKey};
use crate::typed_api::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::{DbError, DbResult};

/// Type alias for outbound route tuples returned by `collect_outbound_routes`
///
/// Each tuple contains:
/// - `String` - Topic/key from the URL path
/// - `Box<dyn ConsumerTrait>` - Callback to create a consumer for this record
/// - `SerializerFn` - User-provided serializer for the record type
/// - `Vec<(String, String)>` - Configuration options from the URL query
#[cfg(feature = "alloc")]
type OutboundRoute = (
    String,
    Box<dyn crate::connector::ConsumerTrait>,
    crate::connector::SerializerFn,
    Vec<(String, String)>,
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
        R: aimdb_executor::Spawn + 'static,
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
        R: aimdb_executor::Spawn + 'static,
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
                let key = &self.keys[i];
                record.collect_metadata(type_id, key.clone(), id)
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
    /// in the `impl<R> where R: Spawn` block, not in `impl AimDbBuilder<NoRuntime>`).
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
        R: aimdb_executor::Spawn + 'static,
    {
        AimDbBuilder {
            records: self.records,
            runtime: Some(rt),
            connector_builders: Vec::new(),
            spawn_fns: Vec::new(),
            #[cfg(feature = "std")]
            remote_config: self.remote_config,
            _phantom: PhantomData,
        }
    }
}

impl<R> AimDbBuilder<R>
where
    R: aimdb_executor::Spawn + 'static,
{
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
                self.records.push((
                    record_key.clone(),
                    type_id,
                    Box::new(TypedRecord::<T, R>::new()),
                ));
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
            #[cfg(feature = "alloc")]
            record_key: record_key.as_str().to_string(),
        };
        f(&mut reg);

        // Only store spawn function for new records to avoid duplicates
        if is_new_record {
            let spawn_key = record_key.clone();

            #[allow(clippy::type_complexity)]
            let spawn_fn: Box<
                dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>, RecordId) -> DbResult<()> + Send,
            > = Box::new(move |runtime: &Arc<R>, db: &Arc<AimDb<R>>, id: RecordId| {
                // Use RecordSpawner to spawn tasks for this record type
                use crate::typed_record::RecordSpawner;

                let typed_record = db.inner().get_typed_record_by_id::<T, R>(id)?;
                // Get the record key from the database to enable key-based producer/consumer
                #[cfg(feature = "alloc")]
                let key = db
                    .inner()
                    .key_for(id)
                    .map(|k| k.as_str().to_string())
                    .unwrap_or_else(|| alloc::format!("__record_{}", id.index()));
                #[cfg(not(feature = "alloc"))]
                let key = "";
                #[cfg(feature = "alloc")]
                let key = key.as_str();
                RecordSpawner::<T>::spawn_all_tasks(typed_record, runtime, db, key)
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

    /// Runs the database indefinitely (never returns)
    ///
    /// This method builds the database, spawns all producer and consumer tasks, and then
    /// parks the current task indefinitely. This is the primary way to run AimDB services.
    ///
    /// All logic runs in background tasks via producers, consumers, and connectors. The
    /// application continues until interrupted (e.g., Ctrl+C).
    ///
    /// # Returns
    /// `DbResult<()>` - Ok when database starts successfully, then parks forever
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
    pub async fn run(self) -> DbResult<()> {
        #[cfg(feature = "tracing")]
        tracing::info!("Building database and spawning background tasks...");

        let _db = self.build().await?;

        #[cfg(feature = "tracing")]
        tracing::info!("Database running, background tasks active. Press Ctrl+C to stop.");

        // Park indefinitely - the background tasks will continue running
        // The database handle is kept alive here to prevent dropping it
        core::future::pending::<()>().await;

        Ok(())
    }

    /// Builds the database and returns the handle (async)
    ///
    /// Use this when you need programmatic access to the database handle for
    /// manual subscriptions or production. For typical services, use `.run().await` instead.
    ///
    /// **Automatic Task Spawning:** This method spawns all producer services and
    /// `.tap()` observer tasks that were registered during configuration.
    ///
    /// **Connector Setup:** Connectors must be created manually before calling `build()`:
    ///
    /// ```rust,ignore
    /// use aimdb_mqtt_connector::{MqttConnector, router::RouterBuilder};
    ///
    /// // Configure records with connector links
    /// let builder = AimDbBuilder::new()
    ///     .runtime(runtime)
    ///     .configure::<Temp>("temp", |reg| {
    ///         reg.link_from("mqtt://commands/temp")
    ///            .with_buffer(BufferCfg::SingleLatest)
    ///            .with_serialization();
    ///     });
    ///
    /// // Create MQTT connector with router
    /// let router = RouterBuilder::new()
    ///     .route("commands/temp", /* deserializer */)
    ///     .build();
    /// let connector = MqttConnector::new("mqtt://localhost:1883", router).await?;
    ///
    /// // Register connector and build
    /// let db = builder
    ///     .with_connector("mqtt", Arc::new(connector))
    ///     .build().await?;
    /// ```
    ///
    /// # Returns
    /// `DbResult<AimDb<R>>` - The database instance
    #[cfg_attr(not(feature = "std"), allow(unused_mut))]
    pub async fn build(self) -> DbResult<AimDb<R>> {
        use crate::DbError;

        // Validate all records
        for (key, _, record) in &self.records {
            record.validate().map_err(|_msg| {
                // Suppress unused warning for key in no_std
                let _ = &key;
                #[cfg(feature = "std")]
                {
                    DbError::RuntimeError {
                        message: format!("Record '{}' validation failed: {}", key.as_str(), _msg),
                    }
                }
                #[cfg(not(feature = "std"))]
                {
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
            by_key.insert(key.clone(), id);
            by_type.entry(type_id).or_default().push(id);
            types.push(type_id);
            keys.push(key);
        }

        let inner = Arc::new(AimDbInner {
            storages,
            by_key,
            by_type,
            types,
            keys,
        });

        let db = Arc::new(AimDb {
            inner: inner.clone(),
            runtime: runtime.clone(),
        });

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Spawning producer services and tap observers for {} records",
            self.spawn_fns.len()
        );

        // Execute spawn functions for each record
        for (key, spawn_fn_any) in self.spawn_fns {
            // Resolve key to RecordId
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

            // Downcast from Box<dyn Any> back to the concrete spawn function type
            type SpawnFnType<R> =
                Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>, RecordId) -> DbResult<()> + Send>;

            let spawn_fn = spawn_fn_any
                .downcast::<SpawnFnType<R>>()
                .expect("spawn function type mismatch");

            // Execute the spawn function
            (*spawn_fn)(&runtime, &db, id)?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!("Automatic spawning complete");

        // Spawn remote access supervisor if configured (std only)
        #[cfg(feature = "std")]
        if let Some(remote_cfg) = self.remote_config {
            #[cfg(feature = "tracing")]
            tracing::info!(
                "Spawning remote access supervisor on socket: {}",
                remote_cfg.socket_path.display()
            );

            // Apply security policy to mark writable records
            let writable_keys = remote_cfg.security_policy.writable_records();
            for key_str in writable_keys {
                if let Some(id) = inner.resolve_str(&key_str) {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("Marking record '{}' as writable", key_str);

                    // Mark the record as writable (type-erased call)
                    inner.storages[id.index()].set_writable_erased(true);
                }
            }

            // Spawn the remote supervisor task
            crate::remote::supervisor::spawn_supervisor(db.clone(), runtime.clone(), remote_cfg)?;

            #[cfg(feature = "tracing")]
            tracing::info!("Remote access supervisor spawned successfully");
        }

        // Build connectors from builders (after database is fully constructed)
        // This allows connectors to use collect_inbound_routes() which creates
        // producers tied to this specific database instance
        for builder in self.connector_builders {
            #[cfg(feature = "tracing")]
            let scheme = {
                #[cfg(feature = "std")]
                {
                    builder.scheme().to_string()
                }
                #[cfg(not(feature = "std"))]
                {
                    alloc::string::String::from(builder.scheme())
                }
            };

            #[cfg(feature = "tracing")]
            tracing::debug!("Building connector for scheme: {}", scheme);

            // Build the connector (this spawns tasks as a side effect)
            let _connector = builder.build(&db).await?;

            #[cfg(feature = "tracing")]
            tracing::info!("Connector built and spawned successfully: {}", scheme);
        }

        // Unwrap the Arc to return the owned AimDb
        // This is safe because we just created it and hold the only reference
        let db_owned = Arc::try_unwrap(db).unwrap_or_else(|arc| (*arc).clone());

        Ok(db_owned)
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
pub struct AimDb<R: aimdb_executor::Spawn + 'static> {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime adapter with concrete type
    runtime: Arc<R>,
}

impl<R: aimdb_executor::Spawn + 'static> Clone for AimDb<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

impl<R: aimdb_executor::Spawn + 'static> AimDb<R> {
    /// Internal accessor for the inner state
    ///
    /// Used by adapter crates and internal spawning logic.
    #[doc(hidden)]
    pub fn inner(&self) -> &Arc<AimDbInner> {
        &self.inner
    }

    /// Builds a database with a closure-based builder pattern
    pub async fn build_with(rt: Arc<R>, f: impl FnOnce(&mut AimDbBuilder<R>)) -> DbResult<()> {
        let mut b = AimDbBuilder::new().runtime(rt);
        f(&mut b);
        b.run().await
    }

    /// Spawns a task using the database's runtime adapter
    ///
    /// This method provides direct access to the runtime's spawn capability.
    ///
    /// # Arguments
    /// * `future` - The future to spawn
    ///
    /// # Returns
    /// `DbResult<()>` - Ok if the task was spawned successfully
    pub fn spawn_task<F>(&self, future: F) -> DbResult<()>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future).map_err(DbError::from)?;
        Ok(())
    }

    /// Produces a value to a specific record by key
    ///
    /// Uses O(1) key-based lookup to find the correct record.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    /// * `value` - The value to produce
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// db.produce::<Temperature>("sensors.indoor", indoor_temp).await?;
    /// db.produce::<Temperature>("sensors.outdoor", outdoor_temp).await?;
    /// ```
    pub async fn produce<T>(&self, key: impl AsRef<str>, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        let typed_rec = self.inner.get_typed_record_by_key::<T, R>(key)?;
        typed_rec.produce(value).await;
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
    /// Returns a `Producer<T, R>` bound to a specific record key.
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
    /// indoor_producer.produce(indoor_temp).await?;
    /// outdoor_producer.produce(outdoor_temp).await?;
    /// ```
    pub fn producer<T>(
        &self,
        key: impl Into<alloc::string::String>,
    ) -> crate::typed_api::Producer<T, R>
    where
        T: Send + 'static + Debug + Clone,
    {
        crate::typed_api::Producer::new(Arc::new(self.clone()), key.into())
    }

    /// Creates a type-safe consumer for a specific record by key
    ///
    /// Returns a `Consumer<T, R>` bound to a specific record key.
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
    /// let mut rx = indoor_consumer.subscribe()?;
    /// ```
    pub fn consumer<T>(
        &self,
        key: impl Into<alloc::string::String>,
    ) -> crate::typed_api::Consumer<T, R>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        crate::typed_api::Consumer::new(Arc::new(self.clone()), key.into())
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

    /// Subscribe to record updates as JSON stream (std only)
    ///
    /// Creates a subscription to a record's buffer and forwards updates as JSON
    /// to a bounded channel. This is used internally by the remote access protocol
    /// for implementing `record.subscribe`.
    ///
    /// # Architecture
    ///
    /// Spawns a consumer task that:
    /// 1. Subscribes to the record's buffer using the existing buffer API
    /// 2. Reads values as they arrive
    /// 3. Serializes each value to JSON
    /// 4. Sends JSON values to a bounded channel (with backpressure handling)
    /// 5. Terminates when either:
    ///    - The cancel signal is received (unsubscribe)
    ///    - The channel receiver is dropped (client disconnected)
    ///
    /// # Arguments
    /// * `record_key` - Key of the record to subscribe to
    /// * `queue_size` - Size of the bounded channel for this subscription
    ///
    /// # Returns
    /// `Ok((receiver, cancel_tx))` where:
    /// - `receiver`: Bounded channel receiver for JSON values
    /// - `cancel_tx`: One-shot sender to cancel the subscription
    ///
    /// `Err` if:
    /// - Record not found for the given key
    /// - Record not configured with `.with_serialization()`
    /// - Failed to subscribe to buffer
    ///
    /// # Example (internal use)
    ///
    /// ```rust,ignore
    /// let (mut rx, cancel_tx) = db.subscribe_record_updates("sensor.temp", 100)?;
    ///
    /// // Read events
    /// while let Some(json_value) = rx.recv().await {
    ///     // Forward to client...
    /// }
    ///
    /// // Cancel subscription
    /// let _ = cancel_tx.send(());
    /// ```
    #[cfg(feature = "std")]
    #[allow(unused_variables)] // Variables used only in tracing feature
    pub fn subscribe_record_updates(
        &self,
        record_key: &str,
        queue_size: usize,
    ) -> DbResult<(
        tokio::sync::mpsc::Receiver<serde_json::Value>,
        tokio::sync::oneshot::Sender<()>,
    )> {
        use tokio::sync::{mpsc, oneshot};

        // Find the record by key
        let id = self
            .inner
            .resolve_str(record_key)
            .ok_or_else(|| DbError::RecordKeyNotFound {
                key: record_key.to_string(),
            })?;

        let record = self
            .inner
            .storage(id)
            .ok_or_else(|| DbError::InvalidRecordId { id: id.raw() })?;

        // Subscribe to the record's buffer as JSON stream
        // This will fail if record not configured with .with_serialization()
        let mut json_reader = record.subscribe_json()?;

        // Create channels for the subscription
        let (value_tx, value_rx) = mpsc::channel(queue_size);
        let (cancel_tx, mut cancel_rx) = oneshot::channel();

        // Get metadata for logging
        let type_id = self.inner.types[id.index()];
        let key = self.inner.keys[id.index()].clone();
        let record_metadata = record.collect_metadata(type_id, key, id);
        let runtime = self.runtime.clone();

        // Spawn consumer task that forwards JSON values from buffer to channel
        let spawn_result = runtime.spawn(async move {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Subscription consumer task started for {}",
                record_metadata.name
            );

            // Main event loop: read from buffer and forward to channel
            loop {
                tokio::select! {
                    // Handle cancellation signal
                    _ = &mut cancel_rx => {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("Subscription cancelled");
                        break;
                    }
                    // Read next JSON value from buffer
                    result = json_reader.recv_json() => {
                        match result {
                            Ok(json_val) => {
                                // Send JSON value to subscription channel
                                if value_tx.send(json_val).await.is_err() {
                                    #[cfg(feature = "tracing")]
                                    tracing::debug!("Subscription receiver dropped");
                                    break;
                                }
                            }
                            Err(DbError::BufferLagged { lag_count, .. }) => {
                                // Consumer fell behind - log warning but continue
                                #[cfg(feature = "tracing")]
                                tracing::warn!(
                                    "Subscription for {} lagged by {} messages",
                                    record_metadata.name,
                                    lag_count
                                );
                                // Continue reading - next recv will get latest
                            }
                            Err(DbError::BufferClosed { .. }) => {
                                // Buffer closed (shutdown) - exit gracefully
                                #[cfg(feature = "tracing")]
                                tracing::debug!("Buffer closed for {}", record_metadata.name);
                                break;
                            }
                            Err(e) => {
                                // Other error (shouldn't happen in practice)
                                #[cfg(feature = "tracing")]
                                tracing::error!(
                                    "Subscription error for {}: {:?}",
                                    record_metadata.name,
                                    e
                                );
                                break;
                            }
                        }
                    }
                }
            }

            #[cfg(feature = "tracing")]
            tracing::debug!("Subscription consumer task terminated");
        });

        spawn_result.map_err(DbError::from)?;

        Ok((value_rx, cancel_tx))
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
    /// # Example
    /// ```rust,ignore
    /// // In MqttConnector after db.build()
    /// let routes = db.collect_inbound_routes("mqtt");
    /// let router = RouterBuilder::from_routes(routes).build();
    /// connector.set_router(router).await?;
    /// ```
    #[cfg(feature = "alloc")]
    pub fn collect_inbound_routes(
        &self,
        scheme: &str,
    ) -> Vec<(
        String,
        Box<dyn crate::connector::ProducerTrait>,
        crate::connector::DeserializerFn,
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

                let topic = link.url.resource_id();

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
    #[cfg(feature = "alloc")]
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
                    routes.push((destination, consumer, serializer, link.config.clone()));
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
