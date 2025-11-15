//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;

use alloc::collections::BTreeMap;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc, vec::Vec};

use crate::typed_api::{RecordRegistrar, RecordT};
use crate::typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
use crate::{DbError, DbResult};

/// Marker type for untyped builder (before runtime is set)
pub struct NoRuntime;

/// Internal database state
///
/// Holds the registry of typed records, indexed by `TypeId`.
pub struct AimDbInner {
    /// Map from TypeId to type-erased records (SPMC buffers for internal data flow)
    pub records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
}

impl AimDbInner {
    /// Helper to get a typed record from the registry
    ///
    /// This encapsulates the common pattern of:
    /// 1. Getting TypeId for type T
    /// 2. Looking up the record in the map
    /// 3. Downcasting to the typed record
    pub fn get_typed_record<T, R>(&self) -> DbResult<&TypedRecord<T, R>>
    where
        T: Send + 'static + Debug + Clone,
        R: aimdb_executor::Spawn + 'static,
    {
        use crate::typed_record::AnyRecordExt;

        let type_id = TypeId::of::<T>();

        #[cfg(feature = "std")]
        let record = self.records.get(&type_id).ok_or(DbError::RecordNotFound {
            record_name: core::any::type_name::<T>().to_string(),
        })?;

        #[cfg(not(feature = "std"))]
        let record = self
            .records
            .get(&type_id)
            .ok_or(DbError::RecordNotFound { _record_name: () })?;

        #[cfg(feature = "std")]
        let typed_record = record.as_typed::<T, R>().ok_or(DbError::InvalidOperation {
            operation: "get_typed_record".to_string(),
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
        self.records
            .iter()
            .map(|(type_id, record)| record.collect_metadata(*type_id))
            .collect()
    }

    /// Try to get record's latest value as JSON by record name (std only)
    ///
    /// Searches for a record with the given name and returns its current value
    /// serialized to JSON. Returns `None` if:
    /// - Record not found
    /// - Record not configured with `.with_serialization()`
    /// - No value available in the atomic snapshot
    ///
    /// # Arguments
    /// * `record_name` - The full Rust type name (e.g., "server::Temperature")
    ///
    /// # Returns
    /// `Some(JsonValue)` with the current record value, or `None`
    #[cfg(feature = "std")]
    pub fn try_latest_as_json(&self, record_name: &str) -> Option<serde_json::Value> {
        for (type_id, record) in &self.records {
            let metadata = record.collect_metadata(*type_id);
            if metadata.name == record_name {
                return record.latest_json();
            }
        }
        None
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
    /// * `record_name` - The full Rust type name (e.g., "server::AppConfig")
    /// * `json_value` - JSON representation of the value
    ///
    /// # Returns
    /// - `Ok(())` - Successfully set the value
    /// - `Err(DbError)` - If record not found, has producers, or deserialization fails
    ///
    /// # Errors
    /// - `RecordNotFound` - Record with given name doesn't exist
    /// - `PermissionDenied` - Record has active producers (safety check)
    /// - `RuntimeError` - Record not configured with `.with_serialization()`
    /// - `JsonWithContext` - JSON deserialization failed (schema mismatch)
    ///
    /// # Example (internal use - called by remote access protocol)
    /// ```rust,ignore
    /// let json_val = serde_json::json!({"log_level": "debug", "version": "1.0"});
    /// db.set_record_from_json("server::AppConfig", json_val)?;
    /// ```
    #[cfg(feature = "std")]
    pub fn set_record_from_json(
        &self,
        record_name: &str,
        json_value: serde_json::Value,
    ) -> DbResult<()> {
        // Find the record by name
        for (type_id, record) in &self.records {
            let metadata = record.collect_metadata(*type_id);
            if metadata.name == record_name {
                // Delegate to the type-erased set_from_json method
                // which will enforce the "no producer override" rule
                return record.set_from_json(json_value);
            }
        }

        // Record not found
        Err(DbError::RecordNotFound {
            record_name: record_name.to_string(),
        })
    }
}

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
/// Use `.runtime()` to set the runtime and transition to a typed builder.
pub struct AimDbBuilder<R = NoRuntime> {
    /// Registry of typed records
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,

    /// Runtime adapter
    runtime: Option<Arc<R>>,

    /// Connector builders that will be invoked during build()
    connector_builders: Vec<Box<dyn crate::connector::ConnectorBuilder<R>>>,

    /// Spawn functions indexed by TypeId
    spawn_fns: BTreeMap<TypeId, Box<dyn core::any::Any + Send>>,

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
            records: BTreeMap::new(),
            runtime: None,
            connector_builders: Vec::new(),
            spawn_fns: BTreeMap::new(),
            #[cfg(feature = "std")]
            remote_config: None,
            _phantom: PhantomData,
        }
    }

    /// Sets the runtime adapter
    ///
    /// This transitions the builder from untyped to typed with concrete runtime `R`.
    pub fn runtime<R>(self, rt: Arc<R>) -> AimDbBuilder<R>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        AimDbBuilder {
            records: self.records,
            runtime: Some(rt),
            connector_builders: Vec::new(),
            spawn_fns: BTreeMap::new(),
            #[cfg(feature = "std")]
            remote_config: None,
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

    /// Configures a record type manually
    ///
    /// Low-level method for advanced use cases. Most users should use `register_record` instead.
    pub fn configure<T>(
        &mut self,
        f: impl for<'a> FnOnce(&'a mut RecordRegistrar<'a, T, R>),
    ) -> &mut Self
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        let entry = self
            .records
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(TypedRecord::<T, R>::new()));

        let rec = entry
            .as_typed_mut::<T, R>()
            .expect("type mismatch in record registry");

        let mut reg = RecordRegistrar {
            rec,
            connector_builders: &self.connector_builders,
        };
        f(&mut reg);

        // Store a spawn function that captures the concrete type T and connectors
        let type_id = TypeId::of::<T>();

        #[allow(clippy::type_complexity)]
        let spawn_fn: Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>) -> DbResult<()> + Send> =
            Box::new(move |runtime: &Arc<R>, db: &Arc<AimDb<R>>| {
                // Use RecordSpawner to spawn tasks for this record type
                use crate::typed_record::RecordSpawner;

                let typed_record = db.inner().get_typed_record::<T, R>()?;
                RecordSpawner::<T>::spawn_all_tasks(typed_record, runtime, db)
            });

        // Store the spawn function (type-erased in Box<dyn Any>)
        self.spawn_fns.insert(type_id, Box::new(spawn_fn));

        self
    }

    /// Registers a self-registering record type
    ///
    /// The record type must implement `RecordT<R>`.
    pub fn register_record<T>(&mut self, cfg: &T::Config) -> &mut Self
    where
        T: RecordT<R>,
    {
        self.configure::<T>(|reg| T::register(reg, cfg))
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
    ///     .configure::<Temp>(|reg| {
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
        for record in self.records.values() {
            record.validate().map_err(|_msg| {
                #[cfg(feature = "std")]
                {
                    DbError::RuntimeError {
                        message: format!("Record validation failed: {}", _msg),
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

        let inner = Arc::new(AimDbInner {
            records: self.records,
        });

        let db = Arc::new(AimDb {
            inner: inner.clone(),
            runtime: runtime.clone(),
        });

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Spawning producer services and tap observers for {} record types",
            self.spawn_fns.len()
        );

        // Execute spawn functions for each record type
        for (_type_id, spawn_fn_any) in self.spawn_fns {
            // Downcast from Box<dyn Any> back to the concrete spawn function type
            type SpawnFnType<R> = Box<dyn FnOnce(&Arc<R>, &Arc<AimDb<R>>) -> DbResult<()> + Send>;

            let spawn_fn = spawn_fn_any
                .downcast::<SpawnFnType<R>>()
                .expect("spawn function type mismatch");

            // Execute the spawn function
            (*spawn_fn)(&runtime, &db)?;
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
            let writable_type_ids = remote_cfg.security_policy.writable_records();
            for (type_id, record) in inner.records.iter() {
                if writable_type_ids.contains(type_id) {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("Marking record {:?} as writable", type_id);

                    // Mark the record as writable (type-erased call)
                    record.set_writable_erased(true);
                }
            }

            // Spawn the remote supervisor task
            // This will be implemented in Task 6
            crate::remote::supervisor::spawn_supervisor(db.clone(), runtime.clone(), remote_cfg)?;

            #[cfg(feature = "tracing")]
            tracing::info!("Remote access supervisor spawned successfully");
        }

        // Build connectors from builders (after database is fully constructed)
        // This allows connectors to use collect_inbound_routes() which creates
        // producers tied to this specific database instance
        let mut built_connectors = BTreeMap::new();
        for builder in self.connector_builders {
            #[cfg(feature = "std")]
            let scheme = builder.scheme().to_string();

            #[cfg(not(feature = "std"))]
            let scheme = alloc::string::String::from(builder.scheme());

            #[cfg(feature = "tracing")]
            tracing::debug!("Building connector for scheme: {}", scheme);

            let connector = builder.build(&db).await?;
            built_connectors.insert(scheme.clone(), connector);

            #[cfg(feature = "tracing")]
            tracing::info!("Connector built and spawned successfully: {}", scheme);
        }

        // Create outbound consumers for all records with connector links
        // Now that connectors are built, we can capture them in the consumer closures
        for (_type_id, record) in inner.records.iter() {
            record.spawn_outbound_consumers(
                &runtime as &dyn core::any::Any,
                &db as &dyn core::any::Any,
                &built_connectors as &dyn core::any::Any,
            )?;
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

    /// Produces a value for a record type
    ///
    /// Writes the value to the record's buffer and triggers all consumers.
    pub async fn produce<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Get the typed record using the helper
        let typed_rec = self.inner.get_typed_record::<T, R>()?;

        // Produce the value directly to the buffer
        typed_rec.produce(value).await;
        Ok(())
    }

    /// Subscribes to a record type's buffer
    ///
    /// Creates a subscription to the configured buffer for the given record type.
    /// Returns a boxed reader for receiving values asynchronously.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut reader = db.subscribe::<Temperature>()?;
    ///
    /// loop {
    ///     match reader.recv().await {
    ///         Ok(temp) => println!("Temperature: {:.1}°C", temp.celsius),
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub fn subscribe<T>(&self) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Get the typed record using the helper
        let typed_rec = self.inner.get_typed_record::<T, R>()?;

        // Subscribe to the buffer
        typed_rec.subscribe()
    }

    /// Creates a type-safe producer for a specific record type
    ///
    /// Returns a `Producer<T, R>` that can only produce values of type `T`.
    /// This is the recommended way to pass database access to producer services,
    /// following the principle of least privilege.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = builder.build()?;
    /// let temp_producer = db.producer::<Temperature>();
    ///
    /// // Pass to service - it can only produce Temperature values
    /// runtime.spawn(temperature_service(ctx, temp_producer)).unwrap();
    /// ```
    pub fn producer<T>(&self) -> crate::typed_api::Producer<T, R>
    where
        T: Send + 'static + Debug + Clone,
    {
        crate::typed_api::Producer::new(Arc::new(self.clone()))
    }

    /// Creates a type-safe consumer for a specific record type
    ///
    /// Returns a `Consumer<T, R>` that can only subscribe to values of type `T`.
    /// This is the recommended way to pass database access to consumer services,
    /// following the principle of least privilege.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = builder.build()?;
    /// let temp_consumer = db.consumer::<Temperature>();
    ///
    /// // Pass to service - it can only consume Temperature values
    /// runtime.spawn(temperature_monitor(ctx, temp_consumer)).unwrap();
    /// ```
    pub fn consumer<T>(&self) -> crate::typed_api::Consumer<T, R>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        crate::typed_api::Consumer::new(Arc::new(self.clone()))
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
    /// * `type_id` - TypeId of the record to subscribe to
    /// * `queue_size` - Size of the bounded channel for this subscription
    ///
    /// # Returns
    /// `Ok((receiver, cancel_tx))` where:
    /// - `receiver`: Bounded channel receiver for JSON values
    /// - `cancel_tx`: One-shot sender to cancel the subscription
    ///
    /// `Err` if:
    /// - Record not found for the given TypeId
    /// - Record not configured with `.with_serialization()`
    /// - Failed to subscribe to buffer
    ///
    /// # Example (internal use)
    ///
    /// ```rust,ignore
    /// let type_id = TypeId::of::<Temperature>();
    /// let (mut rx, cancel_tx) = db.subscribe_record_updates(type_id, 100)?;
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
        type_id: TypeId,
        queue_size: usize,
    ) -> DbResult<(
        tokio::sync::mpsc::Receiver<serde_json::Value>,
        tokio::sync::oneshot::Sender<()>,
    )> {
        use tokio::sync::{mpsc, oneshot};

        // Find the record by TypeId
        let record = self
            .inner
            .records
            .get(&type_id)
            .ok_or(DbError::RecordNotFound {
                record_name: format!("TypeId({:?})", type_id),
            })?;

        // Subscribe to the record's buffer as JSON stream
        // This will fail if record not configured with .with_serialization()
        let mut json_reader = record.subscribe_json()?;

        // Create channels for the subscription
        let (value_tx, value_rx) = mpsc::channel(queue_size);
        let (cancel_tx, mut cancel_rx) = oneshot::channel();

        // Get metadata for logging
        let record_metadata = record.collect_metadata(type_id);
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
    #[cfg(feature = "std")]
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

        for record in self.inner.records.values() {
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
}
