# Changelog - aimdb-core

All notable changes to the `aimdb-core` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Transform API (Design 020)**: Reactive data transformations between records
  - **Single-Input Transforms**: `transform_raw()` method on `RecordRegistrar` for creating reactive derivations
  - **Multi-Input Joins**: `transform_join_raw()` for combining multiple input records with stateful handlers
  - **`TransformBuilder`**: Fluent API with `.with_state()` and `.map()` for transform configuration
  - **`JoinBuilder`**: Multi-input builder with `.input::<T>(key)` and `.with_state().on_trigger()` pattern
  - **`TransformDescriptor`**: Type-erased descriptor for storing transform configuration
  - **`JoinTrigger`**: Event type for multi-input join handlers with index and type-erased value
  - Transforms are spawned as tasks during `AimDb::build()` and subscribe to input buffers
  - Mutual exclusion enforced: a record cannot have both `.source()` and `.transform()`
  - Full tracing integration for transform lifecycle events
- **Graph Introspection API (Design 021)**: Dependency graph visualization and introspection
  - **`RecordOrigin` enum**: Classifies record data sources (`Source`, `Link`, `Transform`, `TransformJoin`, `Passive`)
  - **`GraphNode` struct**: Node metadata including origin, buffer config, tap count, outbound status
  - **`GraphEdge` struct**: Directed edge with `from`, `to`, and edge type classification
  - **`DependencyGraph` struct**: Full graph with nodes, edges, and topological ordering
  - New `AnyRecord` trait methods: `has_transform()`, `record_origin()`, `buffer_info()`, `transform_input_keys()`
  - `RecordId::new()` now accepts `RecordOrigin` parameter for accurate metadata
  - Graph methods in `AimDbInner`: `build_dependency_graph()`, `graph_nodes()`, `graph_edges()`, `graph_topo_order()`
- **Record Drain API (Design 019)**: Non-blocking batch history access
  - **`try_recv()` on BufferReader**: Non-blocking receive returning `Ok(T)`, `Err(BufferEmpty)`, or `Err(BufferLagged)`
  - **`try_recv_json()` on JsonBufferReader**: JSON-serialized non-blocking receive for remote access
  - **`record.drain` AimX protocol method**: Drain accumulated values since last call with optional limit
  - Cold-start semantics: first drain creates reader and returns empty
  - Supports `SpmcRing` (full history), `SingleLatest` (at most 1), `Mailbox` (at most 1)
  - Handler maintains per-connection drain readers via `ConnectionState`
- **Extension Macro**: `impl_record_registrar_ext!` macro in `ext_macros.rs` for generating runtime adapter extension traits

### Changed

- **Renamed `.with_serialization()` to `.with_remote_access()`**: Clearer naming for JSON serialization configuration
- **`RecordId` constructor**: Now requires `RecordOrigin` parameter for dependency graph support
- **`set_from_json` protection**: Now also rejects writes on records with active transforms (in addition to sources)

- **Dynamic Topic/Destination Routing (Design 018)**: Complete support for dynamic topic resolution
  - **Outbound (`TopicProvider` trait)**: Dynamically determine MQTT topics or KNX group addresses based on data being published
    - New `TopicProvider<T>` trait for type-safe topic determination
    - New `TopicProviderAny` trait for type-erased storage
    - New `TopicProviderWrapper<T, P>` struct for type erasure
    - New `TopicProviderFn` type alias for stored providers
    - New `topic_provider` field in `ConnectorLink` struct
    - New `with_topic_provider()` method on `OutboundConnectorBuilder`
    - `OutboundRoute` tuple now includes optional `TopicProviderFn`
  - **Inbound (`TopicResolverFn`)**: Late-binding topic resolution at connector startup
    - New `TopicResolverFn` type alias for closure-based topic resolution
    - New `topic_resolver` field in `InboundConnectorLink` struct
    - New `with_topic_resolver()` method on `InboundConnectorBuilder`
    - New `resolve_topic()` method on `InboundConnectorLink`
    - `collect_inbound_routes()` now resolves topics dynamically at route collection time
  - Enables runtime topic determination from smart contracts, service discovery, or configuration
  - Works in both `std` and `no_std + alloc` environments
- **Unit Tests**: Added comprehensive tests for `TopicProvider` and `TopicResolverFn`

### Changed

- **Outbound Route Collection**: `collect_outbound_routes()` now returns `OutboundRoute` tuples with optional `TopicProviderFn`
- **Inbound Topic Resolution**: `collect_inbound_routes()` now calls `link.resolve_topic()` instead of `link.url.resource_id()` directly, enabling dynamic topic resolution

## [0.4.0] - 2025-12-25

### Added

- **RecordKey Trait (Issue #65)**: `RecordKey` is now a trait instead of a struct
  - Enables user-defined enum keys with `#[derive(RecordKey)]` for compile-time safety
  - `as_str()` method for string representation
  - `link_address()` method for connector metadata (MQTT topics, KNX addresses)
  - `Borrow<str>` bound for O(1) HashMap lookups by string
  - Blanket implementation for `&'static str`
- **StringKey Type**: New struct replacing old `RecordKey` struct
  - `Static(&'static str)` variant for zero-allocation keys
  - `Interned(&'static str)` variant using `Box::leak` for O(1) Copy/Clone
  - Implements `RecordKey` trait
- **derive Feature**: New feature flag enabling `#[derive(RecordKey)]` macro via `aimdb-derive`

### Changed

- **Breaking: RecordKey struct → trait**: The `RecordKey` struct is now a trait. Use `StringKey` for string-based keys or define custom enum keys with `#[derive(RecordKey)]`
- **StringKey Memory Model**: Dynamic keys now use `Box::leak` (interning) instead of `Arc<str>`. This optimizes for O(1) cloning at the cost of never freeing dynamic key memory (acceptable for startup-time registration pattern)

### Migration Guide

**RecordKey is now a trait (breaking change)**

If you were using `RecordKey` directly, switch to `StringKey`:

```rust
// Before (this PR)
use aimdb_core::RecordKey;
let key: RecordKey = "sensors.temp".into();

// After
use aimdb_core::StringKey;
let key: StringKey = "sensors.temp".into();
```

**For compile-time safe keys (recommended for embedded)**

Use the new derive macro:

```rust
use aimdb_core::RecordKey;  // Now a trait, re-exported from aimdb-derive

#[derive(RecordKey, Clone, Copy, PartialEq, Eq)]
pub enum AppKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
}

// Compile-time typo detection!
let producer = db.producer::<Temperature>(AppKey::TempIndoor);
```

**Note on StringKey memory model**

`StringKey::intern()` leaks memory intentionally for O(1) Copy/Clone. This is
designed for startup-time registration (<1000 keys). In debug builds, a
warning fires if you exceed 1000 interned keys.

## [0.3.0] - 2025-12-15

### Added

- **RecordId + RecordKey Architecture (Issue #60)**: Complete rewrite of internal storage for stable record identification
  - `RecordId`: u32 index wrapper for O(1) Vec-based hot-path access
  - `StringKey`: Hybrid `&'static str` / interned with `Borrow<str>` for zero-alloc static keys and flexible dynamic keys
  - O(1) key resolution via `HashMap<RecordKey, RecordId>`
  - Type introspection via `HashMap<TypeId, Vec<RecordId>>`
- **Key-Based Producer/Consumer API**:
  - `produce::<T>(key, value)`: Produce to specific record by key
  - `subscribe::<T>(key)`: Subscribe to specific record by key  
  - `producer::<T>(key)`: Get key-bound producer
  - `consumer::<T>(key)`: Get key-bound consumer
- **New Types**: `Producer<T, R>` and `Consumer<T, R>` for key-bound access with `.key()` accessor
- **Introspection Methods**:
  - `records_of_type::<T>()`: Returns `&[RecordId]` for all records of type T
  - `resolve_key(key)`: O(1) lookup returning `Option<RecordId>`
- **New Error Variants**:
  - `RecordKeyNotFound`: Key doesn't exist in registry
  - `InvalidRecordId`: RecordId out of bounds  
  - `TypeMismatch`: Type assertion failed during downcast
  - `AmbiguousType`: Multiple records of same type (use key-based API)
  - `DuplicateRecordKey`: Key already registered
- **RecordMetadata Extensions**: Now includes `record_id: u32` and `record_key: String` fields
- **Buffer Metrics API**: New `BufferMetrics` trait and `BufferMetricsSnapshot` struct for buffer introspection (feature-gated behind `metrics`)
  - `produced_count`: Total items pushed to the buffer
  - `consumed_count`: Total items consumed across all readers  
  - `dropped_count`: Total items dropped due to lag (documents per-reader semantics)
  - `occupancy`: Current buffer fill level as `(current, capacity)` tuple
- **DynBuffer Metrics Method**: Added `metrics_snapshot()` method to `DynBuffer` trait (returns `Option<BufferMetricsSnapshot>`)

### Removed

- **Breaking: Legacy Type-Only Methods**: Removed methods that accessed records by type alone:
  - `get_typed_record<T>()`: Use `get_typed_record_by_key::<T>(key)` instead
  - `produce<T>(value)`: Use `produce::<T>(key, value)` instead
  - `subscribe<T>()`: Use `subscribe::<T>(key)` instead
  - `producer<T>()`: Use `producer::<T>(key)` instead
  - `consumer<T>()`: Use `consumer::<T>(key)` instead
  - This eliminates `AmbiguousType` errors - all record access now requires explicit keys

### Changed

- **Breaking: `configure<T>()` Signature**: Now requires key parameter: `configure::<T>("key", |reg| ...)`
- **Breaking: Internal Storage**: Changed from `BTreeMap<TypeId, Box<dyn AnyRecord>>` to:
  - `Vec<Box<dyn AnyRecord>>` for O(1) hot-path access by RecordId
  - `HashMap<RecordKey, RecordId>` for O(1) name lookups
  - `HashMap<TypeId, Vec<RecordId>>` for type introspection
- **Breaking: Key-Based API Only**: All producer/consumer methods now require a record key parameter. This properly supports multi-instance records (e.g., multiple temperature sensors with the same type)
- **SecurityPolicy**: `ReadWrite` variant now uses `HashSet<String>` for writable record keys
- **Dependencies**: Added `hashbrown` for no_std-compatible HashMap with `default-hasher` feature
- **Breaking: DynBuffer No Longer Has Blanket Impl**: The blanket `impl<T, B: Buffer<T>> DynBuffer<T> for B` has been removed. Each adapter now provides its own explicit `DynBuffer` implementation. This enables adapters to provide metrics support via `metrics_snapshot()`. Custom `Buffer<T>` implementations must now also implement `DynBuffer<T>` explicitly.

## [0.2.0] - 2025-11-20

### Added

- **Bidirectional Connector Support**: New `ConnectorBuilder` trait enables connectors to support both outbound (AimDB → External) and inbound (External → AimDB) data flows
- **Type-Erased Router System**: New `Router` and `RouterBuilder` in `src/router.rs` automatically route incoming messages to correct typed producers without manual dispatch
- **Inbound Connector API**: New `.link_from()` method for configuring inbound connections (External → AimDB) with deserializer callbacks
- **Outbound Connector API**: New `.link_to()` method for configuring outbound connections (AimDB → External), replacing generic `.link()`
- **Producer Trait**: New `ProducerTrait` for type-erased producer calls, enabling dynamic routing of messages to different record types
- **Resource ID Extraction**: Added `ConnectorUrl::resource_id()` method to extract protocol-specific resource identifiers (topics, keys, paths)
- **Producer Factory Pattern**: New `ProducerFactoryFn` and `InboundConnectorLink` types for creating producers dynamically at runtime
- **Consumer Trait for Outbound Routing**: New `ConsumerTrait` and `AnyReader` traits in `src/connector.rs` enable type-erased outbound message publishing, mirroring `ProducerTrait` architecture
- **Outbound Route Collection**: Added `AimDb::collect_outbound_routes()` method to gather all configured outbound connectors with their type-erased consumers, serializers, and configs
- **Consumer Factory Pattern**: New `ConsumerFactoryFn` type alias and factory storage in `OutboundConnectorLink` for capturing types at configuration time
- **Type-Erased Consumer Adapter**: Added `TypedAnyReader` in `src/typed_api.rs` implementing `AnyReader` trait for `Consumer<T, R>`

### Changed

- **Breaking: Connector Registration**: Changed from `.with_connector(scheme, instance)` to `.with_connector(builder)` - connectors now registered as builders
- **Breaking: Async Build**: `AimDbBuilder::build()` is now async to support connector initialization during database construction
- **Breaking: ConnectorBuilder Trait**: Updated `build()` signature to take `&AimDb<R>`, enabling route collection via `db.collect_inbound_routes()`
- **Breaking: Sync Bound on Records**: Added `Sync` bound to `AnyRecord` trait and `TypedRecord<T, R>` implementation. Record types must now be `Send + Sync` (previously only `Send`). This enables safe concurrent access from multiple connector tasks in the bidirectional routing system. Types that were `Send` but not `Sync` must be wrapped in `Arc<Mutex<_>>` or `Arc<RwLock<_>>` for interior mutability.
- **Breaking: Connector Naming Refactor**: Renamed connector-related APIs to explicitly distinguish outbound (AimDB → External) from inbound (External → AimDB) flows:
  - `TypedRecord::connectors` field → `outbound_connectors`
  - `TypedRecord::add_connector()` → `add_outbound_connector()`
  - `TypedRecord::connectors()` → `outbound_connectors()`
  - `TypedRecord::connector_count()` → `outbound_connector_count()`
  - `AnyRecord::connector_count()` → `outbound_connector_count()`
  - `AnyRecord::connector_urls()` → `outbound_connector_urls()`
  - `AnyRecord::connectors()` → `outbound_connectors()`
  - `RecordMetadata::connector_count` → `outbound_connector_count`
  - Inbound methods (`inbound_connectors()`, `add_inbound_connector()`) remain unchanged for clarity
- **Deprecated `.link()`**: Generic `.link()` method deprecated in favor of explicit `.link_to()` and `.link_from()`
- **Builder API**: Enhanced builder to validate buffer requirements for inbound connectors at configuration time
- **Breaking: Outbound Consumer Architecture**: Refactored outbound connector system to use trait-based type erasure:
  - Removed: `TypedRecord::spawn_outbound_consumers()` method (automatic spawning)
  - Changed: `OutboundConnectorLink` now stores `consumer_factory: Arc<ConsumerFactoryFn>` instead of direct consumer
  - Changed: Connectors must now implement `spawn_outbound_publishers()` and explicitly call `db.collect_outbound_routes()`
  - Impact: All connectors require code changes to support outbound publishing via the new trait system

### Fixed

- **Memory Management**: Removed `async-trait` dependency that leaked `std` into `no_std` builds, replaced with manual `Pin<Box<dyn Future>>`
- **Type-Erased Routing**: Fixed producer storage in collections by implementing `ProducerTrait` with `Box<dyn Any>` downcasting
- **Buffer Validation**: Added validation ensuring inbound connectors have configured buffers before link creation
- **Consumer Factory Downcasting**: Fixed type downcasting in `typed_api.rs` line 565 - changed from downcasting to `Arc<AimDb<R>>` to downcasting to `AimDb<R>` then wrapping in Arc, resolving "Invalid db type in consumer factory" runtime panic

### Removed

- **Channel Stream**: Removed obsolete channel-based stream abstraction in favor of router-based routing
- **Automatic Outbound Spawning**: Removed `TypedRecord::spawn_outbound_consumers()` method - outbound publisher spawning now explicit via `ConsumerTrait` and `spawn_outbound_publishers()`

## [0.1.0] - 2025-11-06

### Added

- Initial release of AimDB async in-memory database engine
- Type-safe record system using `TypeId`-based routing
- Three buffer types for different data flow patterns:
  - **SPMC Ring Buffer**: High-frequency data streams with bounded memory
  - **SingleLatest**: State synchronization and configuration updates
  - **Mailbox**: Commands and one-shot events
- Producer-consumer model with async task spawning
- Runtime adapter abstraction for cross-platform support
- `no_std` compatibility for embedded targets
- Error handling with comprehensive `DbResult<T>` and `DbError` types
- Remote access protocol (AimX v1) for cross-process introspection
- Connector abstraction for external system integration
- Builder pattern API for database configuration
- Record lifecycle management with type-safe APIs

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
