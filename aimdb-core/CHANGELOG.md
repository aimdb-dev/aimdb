# Changelog - aimdb-core

All notable changes to the `aimdb-core` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Bidirectional Connector Support**: New `ConnectorBuilder` trait enables connectors to support both outbound (AimDB → External) and inbound (External → AimDB) data flows
- **Type-Erased Router System**: New `Router` and `RouterBuilder` in `src/router.rs` automatically route incoming messages to correct typed producers without manual dispatch
- **Inbound Connector API**: New `.link_from()` method for configuring inbound connections (External → AimDB) with deserializer callbacks
- **Outbound Connector API**: New `.link_to()` method for configuring outbound connections (AimDB → External), replacing generic `.link()`
- **Producer Trait**: New `ProducerTrait` for type-erased producer calls, enabling dynamic routing of messages to different record types
- **Resource ID Extraction**: Added `ConnectorUrl::resource_id()` method to extract protocol-specific resource identifiers (topics, keys, paths)
- **Producer Factory Pattern**: New `ProducerFactoryFn` and `InboundConnectorLink` types for creating producers dynamically at runtime

### Changed

- **Breaking: Connector Registration**: Changed from `.with_connector(scheme, instance)` to `.with_connector(builder)` - connectors now registered as builders
- **Breaking: Async Build**: `AimDbBuilder::build()` is now async to support connector initialization during database construction
- **Breaking: ConnectorBuilder Trait**: Updated `build()` signature to take `&AimDb<R>`, enabling route collection via `db.collect_inbound_routes()`
- **Deprecated `.link()`**: Generic `.link()` method deprecated in favor of explicit `.link_to()` and `.link_from()`
- **Builder API**: Enhanced builder to validate buffer requirements for inbound connectors at configuration time

### Fixed

- **Memory Management**: Removed `async-trait` dependency that leaked `std` into `no_std` builds, replaced with manual `Pin<Box<dyn Future>>`
- **Type-Erased Routing**: Fixed producer storage in collections by implementing `ProducerTrait` with `Box<dyn Any>` downcasting
- **Buffer Validation**: Added validation ensuring inbound connectors have configured buffers before link creation

### Removed

- **Channel Stream**: Removed obsolete channel-based stream abstraction in favor of router-based routing

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
