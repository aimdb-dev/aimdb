# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: This is the global changelog for the AimDB project. For detailed changes to individual crates, see their respective CHANGELOG.md files:
> - [aimdb-core/CHANGELOG.md](aimdb-core/CHANGELOG.md)
> - [aimdb-derive/CHANGELOG.md](aimdb-derive/CHANGELOG.md)
> - [aimdb-executor/CHANGELOG.md](aimdb-executor/CHANGELOG.md)
> - [aimdb-tokio-adapter/CHANGELOG.md](aimdb-tokio-adapter/CHANGELOG.md)
> - [aimdb-embassy-adapter/CHANGELOG.md](aimdb-embassy-adapter/CHANGELOG.md)
> - [aimdb-mqtt-connector/CHANGELOG.md](aimdb-mqtt-connector/CHANGELOG.md)
> - [aimdb-knx-connector/CHANGELOG.md](aimdb-knx-connector/CHANGELOG.md)
> - [aimdb-sync/CHANGELOG.md](aimdb-sync/CHANGELOG.md)
> - [aimdb-client/CHANGELOG.md](aimdb-client/CHANGELOG.md)
> - [tools/aimdb-cli/CHANGELOG.md](tools/aimdb-cli/CHANGELOG.md)
> - [tools/aimdb-mcp/CHANGELOG.md](tools/aimdb-mcp/CHANGELOG.md)

## [Unreleased]

### Added

- **aimdb-derive**: New crate providing `#[derive(RecordKey)]` macro for compile-time checked record keys
  - `#[key = "..."]` attribute for string representation
  - `#[key_prefix = "..."]` attribute for namespace prefixing
  - `#[link_address = "..."]` attribute for connector metadata (MQTT topics, KNX addresses)
  - Auto-generates `Hash` implementation to satisfy `Borrow<str>` contract
- **aimdb-core**: `RecordKey` is now a **trait** instead of a struct, enabling user-defined enum keys
  - `RecordKey` trait with `as_str()` and optional `link_address()` methods
  - `StringKey` type replaces old `RecordKey` struct (static/interned string keys)
  - `derive` feature flag to enable `#[derive(RecordKey)]` macro
  - Blanket implementation for `&'static str`
- **examples**: New `mqtt-connector-demo-common` and `knx-connector-demo-common` crates for shared cross-platform types and monitors
- **docs**: Added design documents:
  - `016-M6-record-key-trait.md` - RecordKey trait design
  - `017-M6-record-key-hash-impl.md` - Hash implementation for Borrow compliance

### Fixed

- **aimdb-mqtt-connector**: Fixed initialization deadlock when subscribing to >10 MQTT topics (Issue #63)
  - Event loop now spawned before subscribing (spawn-before-subscribe pattern)
  - Dynamic channel capacity scales with topic count (`topics + 10`)

### Changed

- **aimdb-mqtt-connector**: Upgraded `rumqttc` dependency from 0.24 to 0.25
- **deny.toml**: Added `OpenSSL` license allowance (transitive via rumqttc/rustls)
- **deny.toml**: Ignored `RUSTSEC-2025-0134` advisory (rustls-pemfile unmaintained but not vulnerable)

## [0.3.0] - 2025-12-15

### Added

- **aimdb-core**: New `RecordId` type for stable O(1) record indexing (Issue #60)
- **aimdb-core**: New `RecordKey` type for string-based record identification with zero-alloc static keys
- **aimdb-core**: Key-based producer/consumer API: `produce_by_key()`, `subscribe_by_key()`, `producer_by_key()`, `consumer_by_key()`
- **aimdb-core**: `ProducerByKey<T, R>` and `ConsumerByKey<T, R>` types for key-bound access
- **aimdb-core**: `records_of_type::<T>()` introspection method to find all records of a given type
- **aimdb-core**: `resolve_key()` method for O(1) key-to-RecordId lookups
- **aimdb-core**: New error variants: `RecordKeyNotFound`, `InvalidRecordId`, `TypeMismatch`, `AmbiguousType`, `DuplicateRecordKey`
- **aimdb-core**: `RecordMetadata` now includes `record_id` and `record_key` fields
- **aimdb-tokio-adapter**: Multi-instance tests validating same-type different-key scenarios
- **aimdb-core**: New `BufferMetrics` trait and `BufferMetricsSnapshot` struct for buffer introspection (feature-gated behind `metrics`)
  - `produced_count`: Total items pushed to the buffer
  - `consumed_count`: Total items consumed across all readers
  - `dropped_count`: Total items dropped due to lag (with per-reader semantics documented)
  - `occupancy`: Current buffer fill level as `(current, capacity)` tuple
- **aimdb-core**: `RecordMetadata` now includes optional buffer metrics fields when `metrics` feature is enabled
- **aimdb-tokio-adapter**: Full `BufferMetrics` implementation for all buffer types (SPMC Ring, SingleLatest, Mailbox)
- **aimdb-tokio-adapter**: Comprehensive test suite for metrics (`metrics_tests` module)
- **Makefile**: Added test targets for `metrics` feature combinations
- **docs**: Added design document `015-M6-record-id-architecture.md` for future RecordId-based storage

### Changed

- **aimdb-core**: `configure<T>()` now requires a key parameter: `configure::<T>("key", |reg| ...)`
- **aimdb-core**: Internal storage changed from `BTreeMap<TypeId>` to `Vec` + `HashMap` indexes for O(1) hot-path access
- **aimdb-core**: `SecurityPolicy::ReadWrite` now uses `HashSet<String>` for writable record keys
- **aimdb-core**: Added `hashbrown` dependency for no_std-compatible HashMap
- **deny.toml**: Added `Zlib` license allowance (used by `foldhash`, hashbrown's default hasher)
- **aimdb-core**: `DynBuffer` trait no longer has a blanket implementation. Each adapter now provides its own explicit implementation. This enables adapters to provide `metrics_snapshot()` when the `metrics` feature is enabled.

### Migration Guide

**Breaking: `configure<T>()` now requires a key parameter**

The record registration API now requires an explicit key for each record:

```rust
// Before (v0.2.x)
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest);
});

// After (v0.3.0)
builder.configure::<Temperature>("sensor.temperature", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
});
```

**Key naming conventions:**
- Use dot-separated hierarchical names: `"sensors.indoor"`, `"config.app"`
- Keys must be unique across all records (duplicate keys panic at registration)
- For single-instance records, any descriptive string works (e.g., `"sensor.temperature"`, `"app.config"`)

**Breaking: Type-based lookup may return `AmbiguousType` error**

If you register multiple records of the same type, type-based methods (`produce()`, `subscribe()`, `producer()`, `consumer()`) will return `AmbiguousType` error:

```rust
// With multiple Temperature records registered...
db.produce(temp).await  // Returns Err(AmbiguousType { count: 2, ... })

// Use key-based methods instead:
db.produce_by_key("sensors.indoor", temp).await  // Works correctly
```

**New key-based API (recommended for multi-instance scenarios):**

```rust
// Key-bound producer/consumer
let indoor_producer = db.producer_by_key::<Temperature>("sensors.indoor");
let outdoor_producer = db.producer_by_key::<Temperature>("sensors.outdoor");

// Each produces to its own record
indoor_producer.produce(temp).await?;
outdoor_producer.produce(temp).await?;

// Introspection
let temp_ids = db.records_of_type::<Temperature>();  // Returns &[RecordId]
let id = db.resolve_key("sensors.indoor");  // Returns Option<RecordId>
```

**Breaking: DynBuffer no longer has blanket impl**

If you have custom `Buffer<T>` implementations, you now need to also implement `DynBuffer<T>`:

```rust
// Before (v0.2.x) - automatic via blanket impl
impl<T: Clone + Send> Buffer<T> for MyBuffer<T> { ... }
// DynBuffer was automatically implemented

// After (v0.3.0) - explicit implementation required
impl<T: Clone + Send> Buffer<T> for MyBuffer<T> { ... }

impl<T: Clone + Send + 'static> DynBuffer<T> for MyBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }
    
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }
    
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    
    // Optional: implement metrics_snapshot() if you support metrics
    #[cfg(feature = "metrics")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        None // or Some(...) if you track metrics
    }
}
```

## [0.2.0] - 2025-11-20

### Summary

This release introduces **bidirectional connector support**, enabling true two-way data synchronization between AimDB and external systems. The new architecture supports simultaneous publishing and subscribing with automatic message routing, working seamlessly across both Tokio (std) and Embassy (no_std) runtimes.

### Highlights

- 🔄 **Bidirectional Connectors**: New `.link_to()` and `.link_from()` APIs for clear directional data flows
- 🎯 **Type-Erased Router**: Automatic routing of incoming messages to correct typed producers
- 🏗️ **ConnectorBuilder Pattern**: Simplified connector registration with automatic initialization
- 📡 **Enhanced MQTT Connector**: Complete rewrite supporting simultaneous pub/sub with automatic reconnection
- 🌐 **Embassy Network Integration**: Connectors can now access Embassy's network stack for network operations
- 📚 **Comprehensive Guide**: New 1000+ line connector development guide with real-world examples

### Breaking Changes

- **aimdb-core**: `.build()` is now async; connector registration changed from `.with_connector(scheme, instance)` to `.with_connector(builder)`
- **aimdb-core**: `.link()` deprecated in favor of `.link_to()` (outbound) and `.link_from()` (inbound)
- **aimdb-core**: Outbound connector architecture refactored to trait-based system:
  - Removed: `TypedRecord::spawn_outbound_consumers()` method (was called automatically)
  - Added: `ConsumerTrait`, `AnyReader` traits for type-erased outbound routing
  - Added: `AimDb::collect_outbound_routes()` method to gather configured routes
  - **Required**: Connectors must implement `spawn_outbound_publishers()` and call it in `ConnectorBuilder::build()`
- **aimdb-mqtt-connector**: API changed from `MqttConnector::new()` to `MqttConnectorBuilder::new()`; automatic task spawning removes need for manual background task management
- **aimdb-mqtt-connector**: Added `spawn_outbound_publishers()` method; must be called in `build()` for outbound publishing to work

### Modified Crates

See individual changelogs for detailed changes:
- **[aimdb-core](aimdb-core/CHANGELOG.md)**: Core connector architecture, router system, bidirectional APIs
- **[aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md)**: Connector builder integration
- **[aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md)**: Network stack access, connector support
- **[aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md)**: Complete bidirectional rewrite for both runtimes
- **[aimdb-sync](aimdb-sync/CHANGELOG.md)**: Compatibility with async build

### Migration Guide

**1. Update connector registration:**
```rust
// Old (v0.1.0)
let mqtt = MqttConnector::new("mqtt://broker:1883").await?;
builder.with_connector("mqtt", Arc::new(mqtt))

// New (v0.2.0)
builder.with_connector(MqttConnectorBuilder::new("mqtt://broker:1883"))
```

**2. Make build() async:**
```rust
// Old (v0.1.0)
let db = builder.build()?;

// New (v0.2.0)
let db = builder.build().await?;
```

**3. Use directional link methods:**
```rust
// Old (v0.1.0)
.link("mqtt://sensors/temp")

// New (v0.2.0)
.link_to("mqtt://sensors/temp")    // For publishing (AimDB → External)
.link_from("mqtt://commands/temp")  // For subscribing (External → AimDB)
```

**4. Remove manual MQTT task spawning (Embassy):**
```rust
// Old (v0.1.0) - Manual task spawning required
let mqtt_result = MqttConnector::create(...).await?;
spawner.spawn(mqtt_task(mqtt_result.task))?;
builder.with_connector("mqtt", Arc::new(mqtt_result.connector))

// New (v0.2.0) - Automatic task spawning
builder.with_connector(MqttConnectorBuilder::new("mqtt://broker:1883"))
// Tasks spawn automatically during build()
```

**5. Update custom connectors to spawn outbound publishers:**

If you've implemented a custom connector, you **must** add `spawn_outbound_publishers()` support:

```rust
// Old (v0.1.0) - Outbound consumers spawned automatically
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        Ok(Arc::new(MyConnector { /* fields */ }))
    }
    // Outbound publishing happened automatically via TypedRecord::spawn_outbound_consumers()
}

// New (v0.2.0) - Must explicitly spawn outbound publishers
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        let connector = MyConnector { /* fields */ };
        
        // REQUIRED: Collect and spawn outbound publishers
        let outbound_routes = db.collect_outbound_routes(self.protocol_name());
        connector.spawn_outbound_publishers(db, outbound_routes)?;
        
        Ok(Arc::new(connector))
    }
}

// REQUIRED: Implement spawn_outbound_publishers method
impl MyConnector {
    fn spawn_outbound_publishers<R: RuntimeAdapter + 'static>(
        &self,
        db: &AimDb<R>,
        routes: Vec<(String, Box<dyn ConsumerTrait>, SerializerFn, Vec<(String, String)>)>,
    ) -> DbResult<()> {
        for (topic, consumer, serializer, _config) in routes {
            let client = self.client.clone();
            let topic_clone = topic.clone();
            
            db.runtime().spawn(async move {
                // Subscribe to record updates using ConsumerTrait
                match consumer.subscribe_any().await {
                    Ok(mut reader) => {
                        loop {
                            match reader.recv_any().await {
                                Ok(value) => {
                                    // Serialize and publish
                                    if let Ok(bytes) = serializer(&*value) {
                                        let _ = client.publish(&topic_clone, bytes).await;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(_) => { /* Log error */ }
                }
            })?;
        }
        Ok(())
    }
}
```

**Why this change?** The new trait-based architecture provides:
- ✅ Symmetry with inbound routing (`ProducerTrait` ↔ `ConsumerTrait`)
- ✅ Testability (can mock `ConsumerTrait` without real records)
- ✅ Type safety via factory pattern (type capture at configuration time)
- ✅ Maintainability (connector logic stays in connector crate)

**Migration checklist for custom connectors:**
- [ ] Add `spawn_outbound_publishers()` method to connector implementation
- [ ] Call `db.collect_outbound_routes(protocol_name)` in `ConnectorBuilder::build()`
- [ ] Call `connector.spawn_outbound_publishers(db, routes)?` before returning
- [ ] Use `ConsumerTrait::subscribe_any()` to get type-erased readers
- [ ] Handle serialization with provided `SerializerFn`
- [ ] Test both inbound (`.link_from()`) and outbound (`.link_to()`) data flows

See [Connector Development Guide](docs/design/012-M5-connector-development-guide.md) for complete examples.

### Documentation

- Added comprehensive [Connector Development Guide](docs/design/012-M5-connector-development-guide.md)
- Updated MQTT connector examples for both Tokio and Embassy
- Enhanced API documentation with bidirectional patterns

## [0.1.0] - 2025-11-06

### Added

#### Core Database (`aimdb-core`)
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

#### Runtime Adapters
- **Tokio Adapter** (`aimdb-tokio-adapter`): Full-featured std runtime support
  - Lock-free buffer implementations
  - Configurable buffer capacities
  - Comprehensive async task spawning
- **Embassy Adapter** (`aimdb-embassy-adapter`): Embedded `no_std` runtime support
  - Configurable task pool sizes (8/16/32 concurrent tasks)
  - Optimized for resource-constrained devices
  - Compatible with ARM Cortex-M targets

#### MQTT Connector (`aimdb-mqtt-connector`)
- Dual runtime support for both Tokio and Embassy
- Automatic consumer registration via builder pattern
- Topic mapping with QoS and retain configuration
- Pluggable serializers (JSON, MessagePack, Postcard, custom)
- Automatic reconnection handling
- Uses `rumqttc` for std environments
- Uses `mountain-mqtt` for embedded environments

#### Developer Tools
- **MCP Server** (`aimdb-mcp`): LLM-powered introspection and debugging
  - Discover running AimDB instances
  - Query record values and schemas
  - Subscribe to live updates
  - Set writable record values
  - JSON Schema inference from record values
  - Notification persistence to JSONL files
- **CLI Tool** (`aimdb-cli`): Command-line interface (skeleton)
  - Instance discovery and management commands
  - Record inspection capabilities
  - Live watch functionality
- **Client Library** (`aimdb-client`): Reusable connection and discovery logic
  - Unix domain socket communication
  - AimX v1 protocol implementation
  - Clean error handling

#### Synchronous API (`aimdb-sync`)
- Blocking wrapper around async AimDB core
- Thread-safe synchronous record access
- Automatic Tokio runtime management
- Ideal for gradual migration from sync to async

#### Documentation & Examples
- Comprehensive README with architecture overview
- Individual crate documentation with examples
- 12 detailed design documents in `/docs/design`
- Working examples:
  - `tokio-mqtt-connector-demo`: Full Tokio MQTT integration
  - `embassy-mqtt-connector-demo`: Embedded RP2040 with WiFi MQTT
  - `sync-api-demo`: Synchronous API usage patterns
  - `remote-access-demo`: Cross-process introspection server

#### Build & CI Infrastructure
- Comprehensive Makefile with color-coded output
- GitHub Actions workflows:
  - Continuous integration (format, lint, test)
  - Security audits (weekly schedule + on-demand)
  - Documentation generation
  - Release automation
- Cross-compilation testing for `thumbv7em-none-eabihf` target
- `cargo-deny` configuration for license and dependency auditing
- Dev container setup for consistent development environment

### Design Goals Achieved
- ✅ Sub-50ms latency for data synchronization
- ✅ Lock-free buffer operations
- ✅ Cross-platform support (MCU → edge → cloud)
- ✅ Type safety with zero-cost abstractions
- ✅ Protocol-agnostic connector architecture

### Known Limitations
- Kafka and DDS connectors planned for future releases
- CLI tool is currently skeleton implementation
- Performance benchmarks not yet included
- Limited to Unix domain sockets for remote access (no TCP yet)

### Dependencies
- Rust 1.75+ required
- Tokio 1.47+ for std environments
- Embassy 0.9+ for embedded environments
- See `deny.toml` for approved dependency licenses

### Breaking Changes
None (initial release)

### Migration Guide
Not applicable (initial release)

---

## Release Notes

### v0.1.0 - "Foundation Release"

AimDB v0.1.0 establishes the foundational architecture for async, in-memory data synchronization across MCU → edge → cloud environments. This release focuses on core functionality, dual runtime support, and developer tooling.

**Highlights:**
- 🚀 Dual runtime support: Works on both standard library (Tokio) and embedded (Embassy)
- 🔒 Type-safe record system eliminates runtime string lookups
- 📦 Three buffer types cover most data patterns
- 🔌 MQTT connector works in both std and `no_std` environments
- 🤖 MCP server enables LLM-powered introspection
- ✅ 27+ core tests, comprehensive CI/CD, security auditing

**Get Started:**
```bash
cargo add aimdb-core aimdb-tokio-adapter
```

See [README.md](README.md) for quickstart guide and examples.

**Feedback Welcome:**
This is an early release. Please report issues, suggest features, or contribute at:
https://github.com/aimdb-dev/aimdb

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
