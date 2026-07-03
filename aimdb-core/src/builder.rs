//! Database builder with type-safe record registration
//!
//! Provides `AimDb` and `AimDbBuilder` for constructing databases with
//! type-safe, self-registering records using the producer-consumer pattern.

use core::any::TypeId;
use core::fmt::Debug;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use hashbrown::HashMap;

use crate::extensions::Extensions;
use crate::graph::DependencyGraph;

/// Shorthand for a heap-pinned, `Send`, `'static` future — the unit of work
/// the `AimDbRunner` drives. Canonical definition lives in `crate::executor`.
pub type BoxFuture = crate::executor::BoxFuture;

/// `on_start` task stored in `AimDbBuilder::start_fns`, invoked at `build()`.
type StartFnType = Box<dyn FnOnce(crate::RuntimeContext) -> BoxFuture + Send>;

/// Per-record future collector stored in `AimDbBuilder::spawn_fns`.
///
/// At `build()` time each is invoked in topological order; the returned
/// `Vec<BoxFuture>` is appended to the runner's accumulator.
type SpawnFnType = Box<dyn FnOnce(&Arc<AimDb>, RecordId) -> DbResult<Vec<BoxFuture>> + Send>;
use crate::record_id::{RecordId, RecordKey, StringKey};
use crate::typed_api::RecordRegistrar;
use crate::typed_record::{AnyRecord, AnyRecordExt, RecordFutureCollector, TypedRecord};
use crate::{DbError, DbResult};

/// One outbound route returned by [`AimDb::collect_outbound_routes`]
pub struct OutboundRoute {
    /// Default topic/destination from the URL path; used when the source
    /// yields no per-value destination.
    pub topic: String,
    /// Fused wire-level source: its readers yield destination + serialized
    /// payload directly (subscribe → recv → resolve topic → serialize, all
    /// typed inside — no `Box<dyn Any>` per message, design 036 W1).
    pub source: Box<dyn crate::connector::SerializedSource>,
    /// Configuration options from the URL query
    pub config: Vec<(String, String)>,
}

/// One registered record: its key, concrete type, and type-erased storage.
struct RecordEntry {
    key: StringKey,
    type_id: TypeId,
    record: Box<dyn AnyRecord>,
}

/// Internal database state
///
/// The registry is immutable after `build()` and holds tens of records, so
/// it needs exactly two structures: the entries in registration order
/// (indexed by `RecordId`) and a key → id map for name resolution. The hot
/// path (produce/consume) uses pre-resolved handles and never touches the
/// registry; everything else here is control-plane/introspection.
pub struct AimDbInner {
    /// Record entries, indexed by `RecordId`. Order matches registration
    /// order. Immutable after build().
    storages: Vec<RecordEntry>,

    /// Name → RecordId lookup (control plane)
    ///
    /// Used by remote access, CLI, MCP for name resolution.
    by_key: HashMap<StringKey, RecordId>,

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
        self.storages.get(id.index()).map(|e| e.record.as_ref())
    }

    /// Get the StringKey for a given RecordId
    #[inline]
    pub fn key_for(&self, id: RecordId) -> Option<&StringKey> {
        self.storages.get(id.index()).map(|e| &e.key)
    }

    /// Get all RecordIds for a type (introspection)
    ///
    /// Linear scan — the registry is immutable after `build()`, holds tens of
    /// records, and this is called from introspection paths only.
    pub fn records_of_type<T: 'static>(&self) -> Vec<RecordId> {
        let wanted = TypeId::of::<T>();
        self.storages
            .iter()
            .enumerate()
            .filter(|(_, e)| e.type_id == wanted)
            .map(|(i, _)| RecordId::new(i as u32))
            .collect()
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
    pub fn get_typed_record_by_key<T>(&self, key: impl AsRef<str>) -> DbResult<&TypedRecord<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        let key_str = key.as_ref();

        // Resolve key to RecordId
        let id = self
            .resolve_str(key_str)
            .ok_or_else(|| DbError::record_key_not_found(key_str))?;

        self.get_typed_record_by_id::<T>(id)
    }

    /// Helper to get a typed record by RecordId with type validation
    pub fn get_typed_record_by_id<T>(&self, id: RecordId) -> DbResult<&TypedRecord<T>>
    where
        T: Send + 'static + Debug + Clone,
    {
        use crate::typed_record::AnyRecordExt;

        let entry = self
            .storages
            .get(id.index())
            .ok_or(DbError::InvalidRecordId { id: id.raw() })?;

        // The downcast is the type check; keep the descriptive error on failure.
        entry
            .record
            .as_typed::<T>()
            .ok_or_else(|| DbError::TypeMismatch {
                record_id: id.raw(),
                expected_type: core::any::type_name::<T>().to_string(),
            })
    }

    /// Collects metadata for all registered records
    ///
    /// Returns a vector of `RecordMetadata` for remote introspection.
    /// Available only when the `remote-access` feature is enabled.
    #[cfg(feature = "remote-access")]
    pub fn list_records(&self) -> Vec<crate::remote::RecordMetadata> {
        self.storages
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let id = RecordId::new(i as u32);
                e.record.collect_metadata(e.type_id, e.key, id)
            })
            .collect()
    }

    /// Try to get record's latest value as JSON by record key
    ///
    /// O(1) lookup using the key-based index.
    ///
    /// # Arguments
    /// * `record_key` - The record key (e.g., "sensors.temperature")
    ///
    /// # Returns
    /// `Some(JsonValue)` with the current record value, or `None`
    #[cfg(feature = "remote-access")]
    pub fn try_latest_as_json(&self, record_key: &str) -> Option<serde_json::Value> {
        let id = self.resolve_str(record_key)?;
        self.storages
            .get(id.index())?
            .record
            .json_access()?
            .latest_json()
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
    #[cfg(feature = "remote-access")]
    pub fn set_record_from_json(
        &self,
        record_key: &str,
        json_value: serde_json::Value,
    ) -> DbResult<()> {
        let id = self
            .resolve_str(record_key)
            .ok_or_else(|| DbError::record_key_not_found(record_key.to_string()))?;

        self.storages[id.index()]
            .record
            .json_access()
            .ok_or_else(|| {
                DbError::runtime_error(alloc::format!(
                    "Record '{record_key}' does not support JSON remote access"
                ))
            })?
            .set_from_json(json_value)
    }
}

/// Database builder for producer-consumer pattern
///
/// Provides a fluent API for constructing databases with type-safe record registration.
/// Set the runtime via `.runtime()`; a missing runtime is reported by `build()`
/// like any other configuration mistake (issue #133 contract).
pub struct AimDbBuilder {
    /// Registered records with their keys (order matters for RecordId assignment)
    records: Vec<(StringKey, TypeId, Box<dyn AnyRecord>)>,

    /// Key → index into `records`, so repeated `configure()` calls resolve a
    /// key without scanning the vec.
    record_index: HashMap<StringKey, usize>,

    /// Runtime capabilities, held as a value (issue #131)
    runtime: Option<Arc<dyn crate::executor::RuntimeOps>>,

    /// Connector builders that will be invoked during build()
    connector_builders: Vec<Box<dyn crate::connector::ConnectorBuilder>>,

    /// Per-record future collectors with their keys.
    spawn_fns: Vec<(StringKey, SpawnFnType)>,

    /// Startup tasks registered via on_start() — spawned after build() completes.
    start_fns: Vec<StartFnType>,

    /// Generic extension storage for external crates (e.g., persistence, metrics).
    /// Moved into AimDbInner during build() so it can be read on the live AimDb handle.
    extensions: Extensions,

    /// Builder-level configuration mistakes (e.g. re-registering a key with a
    /// different type), recorded instead of panicking and reported — together
    /// with the per-record errors — by `build()` via
    /// [`DbError::InvalidConfiguration`].
    config_errors: Vec<crate::error::ConfigError>,
}

impl AimDbBuilder {
    /// Creates a new database builder without a runtime
    ///
    /// Call `.runtime()` to set the runtime adapter.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            record_index: HashMap::new(),
            runtime: None,
            connector_builders: Vec::new(),
            spawn_fns: Vec::new(),
            start_fns: Vec::new(),
            extensions: Extensions::new(),
            config_errors: Vec::new(),
        }
    }

    /// Sets the runtime adapter.
    ///
    /// Accepts any adapter implementing the dyn-safe
    /// [`RuntimeOps`](crate::executor::RuntimeOps) capability surface
    /// (`TokioAdapter`, `EmbassyAdapter`, `WasmAdapter`); the builder stores it
    /// as a value — no runtime type parameter (issue #131).
    pub fn runtime(mut self, rt: Arc<impl crate::executor::RuntimeOps + 'static>) -> Self {
        self.runtime = Some(rt);
        self
    }
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
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) and
    /// must return a future that runs for as long as needed (e.g. an infinite
    /// cleanup loop). Tasks are spawned in registration order, after all
    /// record tasks and connectors have been started.
    pub fn on_start<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(crate::RuntimeContext) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        self.start_fns.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    /// Registers a connector builder, invoked during [`build`](Self::build).
    ///
    /// This single entry point registers **two kinds** of connector — both
    /// implement [`ConnectorBuilder`](crate::connector::ConnectorBuilder) and are
    /// driven the same way:
    ///
    /// 1. **Data-plane links** (MQTT / KNX / WebSocket): a record opts in with
    ///    `link_to("<scheme>://<topic>")` / `link_from(...)`, and the connector
    ///    mirrors that record to/from the external topic. The connector's
    ///    [`scheme`](crate::connector::ConnectorBuilder::scheme) is what those
    ///    links match against.
    /// 2. **Remote-access session connectors** (UDS / serial / TCP): these expose
    ///    AimDB itself over a transport so peers can introspect/subscribe/write.
    ///    - The **client** half (e.g. `UdsClient`) dials a peer and *does* use
    ///      `link_to`/`link_from` under its scheme — just like (1), the scheme is
    ///      `"uds"` by default instead of `"mqtt"`.
    ///    - The **server** half (e.g. `UdsServer`) *accepts* connections and takes
    ///      **no links** — registering it is how a server stands up remote access
    ///      (this replaces the old `with_remote_access(config)`).
    ///
    /// # Examples
    ///
    /// Illustrative (not compiled: the connector types live in downstream
    /// crates `aimdb-core` cannot depend on):
    ///
    /// ```rust,ignore
    /// // (1) data-plane link to an MQTT topic
    /// let mut b = AimDbBuilder::new().runtime(rt)
    ///     .with_connector(MqttConnector::new("mqtt://broker.local:1883"));
    /// b.configure::<Temperature>("commands.temp", |r| {
    ///     r.link_from("mqtt://commands/temp").with_deserializer_raw(parse).finish();
    /// });
    /// b.build().await?;
    ///
    /// // (2a) remote-access SERVER — no links, just expose this db over UDS
    /// AimDbBuilder::new().runtime(rt)
    ///     .with_connector(UdsServer::from_config(remote_config))
    ///     .build().await?;
    ///
    /// // (2b) remote-access CLIENT — mirror a record to a peer over UDS
    /// let mut b = AimDbBuilder::new().runtime(rt)
    ///     .with_connector(UdsClient::new("/run/aimdb.sock"));
    /// b.configure::<Temp>("temp", |r| { r.with_remote_access().link_to("uds://temp"); });
    /// b.build().await?;
    /// ```
    pub fn with_connector(
        mut self,
        builder: impl crate::connector::ConnectorBuilder + 'static,
    ) -> Self {
        self.connector_builders.push(Box::new(builder));
        self
    }

    // NOTE: a remote-access **server** is registered like any other connector —
    // there is no dedicated builder method:
    //
    //     .with_connector(aimdb_uds_connector::UdsServer::from_config(config))
    //
    // This rides the `with_connector` spine (see its docs) and lets the transport
    // be swapped (UDS / serial / TCP) without touching the builder. The per-record
    // `TypedRecord::with_remote_access()` is unrelated.

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
    pub fn configure<T>(
        &mut self,
        key: impl RecordKey,
        f: impl FnOnce(&mut RecordRegistrar<'_, T>),
    ) -> &mut Self
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        // Convert any RecordKey to StringKey for internal storage
        let record_key: StringKey = StringKey::from_dynamic(key.as_str());
        let type_id = TypeId::of::<T>();

        // Find existing record with this key, or create new one
        let record_index = self.record_index.get(&record_key).copied();

        let (rec, is_new_record) = match record_index {
            Some(idx) => {
                // Use existing record. A key re-registered with a different
                // type is a user mistake: record it (reported from build())
                // and skip the closure — a registrar for the wrong `T`
                // cannot even be constructed.
                let (_, existing_type, record) = &mut self.records[idx];
                if *existing_type != type_id {
                    self.config_errors.push(crate::error::ConfigError::new(
                        record_key.as_str(),
                        None,
                        "key already registered with a different type",
                    ));
                    return self;
                }
                (
                    record.as_typed_mut::<T>().expect(
                        "record registry type mismatch despite TypeId check — \
                         this is a bug in aimdb-core",
                    ),
                    false,
                )
            }
            None => {
                // Create new record
                self.record_index.insert(record_key, self.records.len());
                self.records
                    .push((record_key, type_id, Box::new(TypedRecord::<T>::new())));
                let (_, _, record) = self.records.last_mut().unwrap();
                (
                    record.as_typed_mut::<T>().expect(
                        "record registry type mismatch despite TypeId check — \
                         this is a bug in aimdb-core",
                    ),
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

            let spawn_fn: SpawnFnType = Box::new(move |db: &Arc<AimDb>, id: RecordId| {
                let typed_record = db.inner().get_typed_record_by_id::<T>(id)?;
                // Resolve the record's key for key-based Producer/Consumer construction.
                let key = db
                    .inner()
                    .key_for(id)
                    .map(|k| k.as_str().to_string())
                    .unwrap_or_else(|| alloc::format!("__record_{}", id.index()));
                RecordFutureCollector::<T>::collect_all_futures(typed_record, db, key.as_str())
            });

            self.spawn_fns.push((spawn_key, spawn_fn));
        }

        self
    }

    /// Builds the database and drives every collected future to completion.
    ///
    /// Convenience wrapper for the common case: call `build()`, then immediately
    /// `runner.run().await`. The database handle is dropped on exit.
    ///
    /// For programmatic access to the handle (manual subscriptions, holding the
    /// `AimDb` for other uses), prefer `build()` directly.
    ///
    /// # Returns
    /// `DbResult<()>` — Ok once the database starts; the call then blocks until
    /// every future the runner is driving has completed (typically forever).
    pub async fn run(self) -> DbResult<()> {
        log_info!("Building database and spawning background tasks...");

        let (_db, runner) = self.build().await?;

        log_info!("Database running, runner driving collected futures.");

        runner.run().await;

        Ok(())
    }

    /// Builds the database and returns a `(handle, runner)` pair.
    ///
    /// `build()` collects every future the database needs to drive (producer
    /// services, consumer taps, transforms with fan-in forwarders, connectors,
    /// remote-access supervisor, `on_start` tasks) into the [`AimDbRunner`]
    /// returned alongside the clone-able [`AimDb`] handle. **No background
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
    /// `DbResult<(AimDb, AimDbRunner)>` — the handle (cloneable) and the
    /// non-`Clone` runner that owns the collected futures.
    ///
    /// # Errors
    ///
    /// `build()` is the single surface for configuration mistakes: everything
    /// recorded during configuration (conflicting `.source()`/`.transform()`/
    /// `.link_from()`, missing serializers, unregistered schemes, …) plus the
    /// build-time checks here (record validation, linked records without a
    /// buffer, duplicate keys, dependency-graph cycles) is collected and
    /// returned as one [`DbError::InvalidConfiguration`] carrying **all**
    /// findings — one run surfaces every mistake.
    pub async fn build(mut self) -> DbResult<(AimDb, AimDbRunner)> {
        use crate::error::ConfigError;

        // ── Validation pass: collect every configuration mistake before any
        // effectful phase runs (issue #133). Nothing returns early here.
        let mut errors = core::mem::take(&mut self.config_errors);

        for (key, _, record) in self.records.iter_mut() {
            // Mistakes recorded during configuration — fill in the record key
            // the setters didn't have.
            for mut e in record.drain_config_errors() {
                if e.record_key.is_empty() {
                    e.record_key = key.as_str().to_string();
                }
                errors.push(e);
            }

            // Writer exclusivity: .source(), .transform(), and .link_from()
            // all write into the record's buffer and would race as
            // last-writer-wins — at most one origin kind per record. Validated
            // once here (the setters only catch same-stage duplicates).
            let mut writers: Vec<&str> = Vec::new();
            if record.has_producer() {
                writers.push(".source()");
            }
            if record.has_transform() {
                writers.push(".transform()");
            }
            if !record.inbound_connectors().is_empty() {
                writers.push(".link_from()");
            }
            if writers.len() > 1 {
                let url = record
                    .inbound_connectors()
                    .first()
                    .map(|l| l.url.to_string());
                errors.push(ConfigError::new(
                    key.as_str(),
                    url,
                    alloc::format!(
                        "conflicting writers: record has {}; these are mutually \
                         exclusive (all write the same buffer)",
                        writers.join(" + ")
                    ),
                ));
            }

            // Connector links subscribe to / produce into the record's buffer;
            // a linked record without one only surfaced at spawn time before.
            let has_links =
                !record.outbound_connectors().is_empty() || !record.inbound_connectors().is_empty();
            if has_links && !record.has_buffer() {
                let url = record
                    .outbound_connectors()
                    .first()
                    .map(|l| l.url.to_string())
                    .or_else(|| {
                        record
                            .inbound_connectors()
                            .first()
                            .map(|l| l.url.to_string())
                    });
                errors.push(ConfigError::new(
                    key.as_str(),
                    url,
                    "linked record requires a buffer (call .buffer(...))",
                ));
            }
        }

        // Ensure runtime is set — a missing runtime is a configuration
        // mistake, collected like every other so it never hides the rest of
        // the findings (issue #133). The runtime is bound at the final check.
        if self.runtime.is_none() {
            errors.push(ConfigError::new(
                "",
                None,
                "runtime not set (use .runtime())",
            ));
        }

        // Build the registry
        let record_count = self.records.len();
        let mut storages: Vec<RecordEntry> = Vec::with_capacity(record_count);
        let mut by_key: HashMap<StringKey, RecordId> = HashMap::with_capacity(record_count);

        for (key, type_id, record) in self.records.into_iter() {
            // Duplicate keys (should not happen if configure() is used
            // correctly): collect and skip so every mistake is reported.
            if by_key.contains_key(&key) {
                errors.push(ConfigError::new(key.as_str(), None, "duplicate record key"));
                continue;
            }

            let id = RecordId::new(storages.len() as u32);
            storages.push(RecordEntry {
                key,
                type_id,
                record,
            });
            by_key.insert(key, id);
        }

        // Build dependency graph nodes straight from the records.
        let graph_nodes: Vec<crate::graph::GraphNode> = storages
            .iter()
            .map(|e| {
                let record = &e.record;
                let (buffer_type, buffer_capacity) = record.buffer_info();

                crate::graph::GraphNode {
                    key: e.key.as_str().to_string(),
                    origin: record.record_origin(),
                    buffer_type,
                    buffer_capacity,
                    tap_count: record.consumer_count(),
                    has_outbound_link: !record.outbound_connectors().is_empty(),
                }
            })
            .collect();

        // Build and validate the dependency graph; fold its findings (cycles,
        // unregistered transform inputs) into the collected errors.
        let dependency_graph = match DependencyGraph::build_and_validate(graph_nodes) {
            Ok(graph) => graph,
            Err(e) => {
                errors.push(ConfigError::new("", None, alloc::format!("{e}")));
                return Err(DbError::InvalidConfiguration { errors });
            }
        };

        // All validation done — report every collected mistake at once. The
        // runtime binding lives here so a missing one is reported alongside
        // every other finding instead of short-circuiting them (issue #133).
        let runtime = match (self.runtime.take(), errors.is_empty()) {
            (Some(rt), true) => rt,
            _ => return Err(DbError::InvalidConfiguration { errors }),
        };

        log_debug!(
            "Dependency graph built successfully ({} nodes, {} edges, topo order: {:?})",
            dependency_graph.nodes.len(),
            dependency_graph.edges.len(),
            dependency_graph.topo_order
        );

        let inner = Arc::new(AimDbInner {
            storages,
            by_key,
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

        log_info!("Collecting futures for {} records", self.spawn_fns.len());

        // Build a lookup map from spawn_fns for topological ordering
        let mut spawn_fn_map: HashMap<StringKey, SpawnFnType> =
            self.spawn_fns.into_iter().collect();

        // Execute collectors in topological order — transforms collect after their inputs.
        // `StringKey: Borrow<str>` with content-based Hash, so the map lookup by
        // `&str` is O(1) and yields both the interned key and the RecordId.
        for key_str in inner.dependency_graph.topo_order() {
            let Some((&key, &id)) = inner.by_key.get_key_value(key_str.as_str()) else {
                continue;
            };

            let Some(spawn_fn) = spawn_fn_map.remove(&key) else {
                continue;
            };

            futures_acc.extend(spawn_fn(&db, id)?);
        }

        log_info!("Record future collection complete");

        // AimX remote-access servers are no longer stood up here: register a
        // session connector (`UdsServer::from_config(...)`) via `with_connector`
        // instead — it collects below like any other connector, binds its
        // transport, applies the security policy's writable marking, and drives
        // the shared session engine. See `with_connector`'s docs.

        // Collect connector futures. After issue #88 connector builders return
        // a `Vec<BoxFuture>` instead of an `Arc<dyn Connector>` (which previously
        // was discarded anyway — see design doc 028 §"The dropped Arc<dyn Connector> object").
        for builder in self.connector_builders {
            log_debug!("Building connector for scheme: {}", builder.scheme());

            let connector_futures = builder.build(&db).await?;
            let n_futures = connector_futures.len();
            futures_acc.extend(connector_futures);

            log_info!(
                "Connector '{}' contributed {} future(s)",
                builder.scheme(),
                n_futures
            );
        }

        // Collect on_start futures (registered by external crates like aimdb-persistence).
        if !self.start_fns.is_empty() {
            log_debug!("Collecting {} on_start future(s)", self.start_fns.len());

            for start_fn in self.start_fns {
                futures_acc.push(start_fn(crate::RuntimeContext::new(runtime.clone())));
            }
        }

        let db_owned = (*db).clone();

        Ok((db_owned, AimDbRunner::new(futures_acc)))
    }
}

impl Default for AimDbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer-consumer database
///
/// A database instance with type-safe record registration and cross-record
/// communication via the Emitter pattern. The runtime is held as a value
/// (`Arc<dyn RuntimeOps>`) — records are the only generic surface (issue #131).
///
/// See `examples/` for usage.
///
/// # Examples
///
/// Illustrative (not compiled: the runtime adapter lives in a downstream
/// crate `aimdb-core` cannot depend on):
///
/// ```rust,ignore
/// use aimdb_tokio_adapter::TokioAdapter;
///
/// let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter::new()?));
/// builder.register_record::<Temperature>(&TemperatureConfig);
/// let (db, runner) = builder.build().await?;
/// ```
#[derive(Clone)]
pub struct AimDb {
    /// Internal state
    inner: Arc<AimDbInner>,

    /// Runtime capabilities, held as a value
    runtime: Arc<dyn crate::executor::RuntimeOps>,

    /// Shared wall clock for stage profiling, built from the runtime at `build()` time.
    #[cfg(feature = "profiling")]
    profiling_clock: crate::profiling::Clock,
}

// ============================================================================
// AimDbRunner — drives every future the builder collected
// ============================================================================

/// Non-`Clone` runner returned alongside [`AimDb`] from
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

impl AimDb {
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
    pub fn extensions(&self) -> &Extensions {
        &self.inner.extensions
    }

    /// Shared wall clock used by stage profiling (nanoseconds since an arbitrary epoch).
    #[cfg(feature = "profiling")]
    pub(crate) fn profiling_clock(&self) -> &crate::profiling::Clock {
        &self.profiling_clock
    }

    /// Builds a database with a closure-based builder pattern
    pub async fn build_with(
        rt: Arc<impl crate::executor::RuntimeOps + 'static>,
        f: impl FnOnce(&mut AimDbBuilder),
    ) -> DbResult<()> {
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
    pub fn produce<T>(&self, key: impl AsRef<str>, value: T) -> DbResult<()>
    where
        T: Send + 'static + Debug + Clone,
    {
        // Single write path via WriteHandle (design 031). For hot paths,
        // prefer `db.producer::<T>(key)` once and reuse the returned handle.
        let typed_rec = self.inner.get_typed_record_by_key::<T>(key)?;
        typed_rec.writer_handle().push(value);
        Ok(())
    }

    /// Subscribes to a specific record by key
    ///
    /// Uses O(1) key-based lookup to find the correct record.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
    pub fn subscribe<T>(&self, key: impl AsRef<str>) -> DbResult<crate::buffer::Reader<T>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        let typed_rec = self.inner.get_typed_record_by_key::<T>(key)?;
        typed_rec.subscribe()
    }

    /// Creates a type-safe producer for a specific record by key
    ///
    /// Returns a `Producer<T>` bound to a specific record key.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
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
        let typed_rec = self.inner.get_typed_record_by_key::<T>(&key_str)?;
        Ok(crate::typed_api::Producer::new(typed_rec.writer_handle()))
    }

    /// Creates a type-safe consumer for a specific record by key
    ///
    /// Returns a `Consumer<T>` bound to a specific record key.
    ///
    /// # Arguments
    /// * `key` - The record key (e.g., "sensor.temperature")
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
        let typed_rec = self.inner.get_typed_record_by_key::<T>(&key_str)?;
        let buffer = typed_rec.buffer_handle().ok_or_else(|| {
            DbError::missing_configuration(alloc::format!("buffer for record '{}'", key_str))
        })?;
        Ok(crate::typed_api::Consumer::new(buffer))
    }

    /// Resolve a record key to its RecordId
    ///
    /// Useful for checking if a record exists before operations.
    pub fn resolve_key(&self, key: &str) -> Option<crate::record_id::RecordId> {
        self.inner.resolve_str(key)
    }

    /// Get all record IDs for a specific type
    ///
    /// Returns a slice of RecordIds for all records of type T.
    /// Useful for introspection when multiple records of the same type exist.
    pub fn records_of_type<T: 'static>(&self) -> Vec<crate::record_id::RecordId> {
        self.inner.records_of_type::<T>()
    }

    /// Returns an owned `Arc` handle to the runtime capabilities.
    ///
    /// Connectors that hand the runtime to a `'static` engine future (e.g. the
    /// session client engine, which needs the clock for reconnect
    /// backoff/keepalive) clone it through here.
    pub fn runtime_ops(&self) -> Arc<dyn crate::executor::RuntimeOps> {
        self.runtime.clone()
    }

    /// Returns a [`RuntimeContext`](crate::RuntimeContext) over this
    /// database's runtime — the value handed to services and context-aware
    /// (de)serializers.
    pub fn runtime_ctx(&self) -> crate::RuntimeContext {
        crate::RuntimeContext::new(self.runtime.clone())
    }

    /// Lists all registered records (std only)
    ///
    /// Returns metadata for all registered records, useful for remote access introspection.
    /// Available only when the `std` feature is enabled.
    #[cfg(feature = "remote-access")]
    pub fn list_records(&self) -> Vec<crate::remote::RecordMetadata> {
        self.inner.list_records()
    }

    /// Resets stage profiling counters for every record (feature `profiling`).
    #[cfg(feature = "profiling")]
    pub fn reset_stage_profiling(&self) {
        for entry in &self.inner.storages {
            entry.record.reset_profiling();
        }
    }

    /// Resets buffer introspection counters for every record (feature `metrics`).
    #[cfg(feature = "metrics")]
    pub fn reset_buffer_metrics(&self) {
        for entry in &self.inner.storages {
            entry.record.reset_buffer_metrics();
        }
    }

    /// Try to get record's latest value as JSON by name
    ///
    /// Convenience wrapper around `AimDbInner::try_latest_as_json()`.
    ///
    /// # Arguments
    /// * `record_name` - The full Rust type name (e.g., "server::Temperature")
    ///
    /// # Returns
    /// `Some(JsonValue)` with the current value, or `None` if the record has no
    /// value yet, no buffer, or a buffer with **no canonical latest** — i.e.
    /// [`SpmcRing`](crate::buffer::BufferCfg::SpmcRing). A ring is a stream/backlog
    /// with no single "current value"; read it via a subscriber or `record.drain`,
    /// not a peek (`record.get`). Use [`SingleLatest`](crate::buffer::BufferCfg::SingleLatest)
    /// for state you want to read latest-value style.
    #[cfg(feature = "remote-access")]
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
    #[cfg(feature = "remote-access")]
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
    /// and returns routes with fused ingest callbacks (deserialize + produce
    /// in one typed closure — no `Box<dyn Any>` per message).
    ///
    /// # Arguments
    /// * `scheme` - URL scheme to filter by (e.g., "mqtt", "kafka")
    ///
    /// # Returns
    /// Vector of tuples: (topic, ingest)
    ///
    /// The topic is resolved dynamically if a `TopicResolverFn` is configured,
    /// otherwise the static topic from the URL is used.
    pub fn collect_inbound_routes(
        &self,
        scheme: &str,
    ) -> Vec<(String, crate::connector::IngestFn)> {
        let mut routes = Vec::new();

        for entry in &self.inner.storages {
            let inbound_links = entry.record.inbound_connectors();

            for link in inbound_links {
                // Filter by scheme
                if link.url.scheme() != scheme {
                    continue;
                }

                // Resolve topic: dynamic (from resolver) or static (from URL)
                let topic = link.resolve_topic();

                // Create the fused ingest callback using the stored factory
                routes.push((topic, link.create_ingest(self)));
            }
        }

        if !routes.is_empty() {
            log_debug!(
                "Collected {} inbound routes for scheme '{}'",
                routes.len(),
                scheme
            );
        }

        routes
    }

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

        for entry in self.inner.storages.iter() {
            let type_id = entry.type_id;

            for link in entry.record.outbound_connectors() {
                if link.url.scheme() != scheme {
                    continue;
                }
                result.push((link.url.resource_id().to_string(), type_id));
            }
        }

        result
    }

    /// Collects outbound routes for a specific protocol scheme
    ///
    /// Mirrors `collect_inbound_routes()` for symmetry. Iterates all records,
    /// filters their outbound_connectors by scheme, and returns
    /// [`OutboundRoute`]s carrying fused serialized sources (subscribe →
    /// recv → resolve topic → serialize, all typed inside — no
    /// `Box<dyn Any>` per message).
    ///
    /// This method is called by connectors during their `build()` phase to
    /// collect all configured outbound routes and spawn publisher tasks
    /// (usually via `pump_sink`).
    ///
    /// # Arguments
    /// * `scheme` - URL scheme to filter by (e.g., "mqtt", "kafka")
    pub fn collect_outbound_routes(&self, scheme: &str) -> Vec<OutboundRoute> {
        let mut routes = Vec::new();

        for entry in &self.inner.storages {
            let outbound_links = entry.record.outbound_connectors();

            for link in outbound_links {
                // Filter by scheme
                if link.url.scheme() != scheme {
                    continue;
                }

                // Create the fused source using the stored factory
                routes.push(OutboundRoute {
                    topic: link.url.resource_id().to_string(),
                    source: link.create_source(self),
                    config: link.config.clone(),
                });
            }
        }

        if !routes.is_empty() {
            log_debug!(
                "Collected {} outbound routes for scheme '{}'",
                routes.len(),
                scheme
            );
        }

        routes
    }
}
