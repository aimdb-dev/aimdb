# Changelog - aimdb-core

All notable changes to the `aimdb-core` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Buffer Metrics API**: New `BufferMetrics` trait and `BufferMetricsSnapshot` struct for buffer introspection (feature-gated behind `metrics`)
  - `produced_count`: Total items pushed to the buffer
  - `consumed_count`: Total items consumed across all readers  
  - `dropped_count`: Total items dropped due to lag (documents per-reader semantics)
  - `occupancy`: Current buffer fill level as `(current, capacity)` tuple
- **RecordMetadata Extensions**: `RecordMetadata` now includes optional buffer metrics fields (`produced_count`, `consumed_count`, `dropped_count`, `occupancy`) when `metrics` feature is enabled
- **DynBuffer Metrics Method**: Added `metrics_snapshot()` method to `DynBuffer` trait (returns `Option<BufferMetricsSnapshot>`)

### Changed

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
