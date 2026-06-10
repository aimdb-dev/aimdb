# Changelog - aimdb-mqtt-connector

All notable changes to the `aimdb-mqtt-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Connector-build errors carry their message on `no_std` too (Issue #129).** With `DbError` unified on `alloc::String`, the dual `#[cfg]` error-construction branches in both clients collapse to one `DbError::runtime_error(...)` expression; the Embassy client's "Failed to build MQTT connector" detail is no longer dropped on embedded targets. No API change.
- **Tokio client rebuilt on the shared data-plane toolkit (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** The hand-rolled consume-serialize-publish and read-route loops are replaced by `aimdb-core`'s `pump_sink` / `pump_source` helpers (the connector now writes only its `Connector`/`Source` I/O adapters and composes the pumps in `build()`). Per-route configuration (`qos` / `retain` / `timeout_ms` / …) is threaded from each link URL's query via `ConnectorConfig::from_query`. `std` now enables `aimdb-core/connector-session` (where the pump helpers live; `std` implies it transitively). No public API change.
- **Outbound publisher survives a consumer lag (Embassy client, Issue #39).** A `BufferLagged` (SPMC-ring overflow) on the outbound reader now skips the gap and keeps publishing instead of terminating the publisher; only a closed buffer stops it.
- **M17 — Embassy client rebuilt on core's pumps via the adapter spine ([Design 033](../docs/design/033-M17-unify-connectors-drop-send.md)).** The hand-rolled outbound publisher and inbound event-router loops are gone: the Embassy half now rides core's `pump_sink` / `pump_source` through the force-`Send` `EmbassySink` / `EmbassySource` bridges in `aimdb-embassy-adapter::connectors`, exactly like the Tokio half rides them — this crate contributes only the broker **manager task** (mountain-mqtt's `run`, force-`Send`ed once via `into_box_future`) and the `MqttSink` / `MqttSource` over its action/event channels. **No `unsafe`, no `SendFutureWrapper`** remain in this crate. Per-route `qos` / `retain` still arrive from each link URL's query (now via `ConnectorConfig::protocol_options`, parsed per publish). Note: per-message inbound routing logs moved from this crate's `defmt` calls into core's `pump_source` (`tracing` feature), so defmt-only MCU builds no longer log per-message routing failures.

### Changed (breaking)

- **`ConnectorBuilder::build()` now returns `Vec<BoxFuture<'static, ()>>` instead of `Arc<dyn Connector>` (Issue #88).** Both Tokio and Embassy implementations updated. The MQTT event-loop, the Embassy event-router, and every outbound publisher are returned as futures that the `AimDbRunner` drives — no more `runtime.spawn` / `tokio::spawn` inside the connector. `R: Spawn` bounds dropped throughout in favour of `R: RuntimeAdapter`.
- `spawn_event_loop()` → `build_event_loop_future()` (Tokio side). `spawn_outbound_publishers()` → `collect_outbound_futures()` on both Tokio and Embassy.
- The `transport::Connector` impl on `MqttConnectorImpl` was removed alongside the discarded `Arc<dyn Connector>` return path; direct programmatic publish was already unreachable through the `AimDbBuilder` public API.
- **`MqttConnectorImpl` (Embassy) removed entirely (M17).** It was a build-time aggregation holder; its logic collapsed into the private `setup_manager` + the pump composition in `build()`. Register via `MqttConnectorBuilder` as before — the builder's public API is unchanged.

## [0.6.0] - 2026-05-22

### Changed

- Updated `Router::route()` calls to pass runtime context via `db.runtime_any()`, enabling context-aware deserializers (Design 026)
- Updated outbound publishers (Tokio and Embassy) to dispatch via `SerializerKind`, enabling context-aware serializers with `db.runtime_any()`

## [0.5.1] - 2026-03-16

### Changed

- Updated Embassy dependency versions: executor 0.10.0, time 0.5.1, sync 0.8.0, net 0.9.0

## [0.5.0] - 2026-02-21

### Added

- **Dynamic Topic Routing (Design 018)**: Full support for dynamic MQTT topic resolution
  - **Outbound**: Uses `TopicProvider` to dynamically determine publish topics based on data values. Configure via `.with_topic_provider()` on outbound connectors.
  - **Inbound**: Uses `TopicResolverFn` for late-binding subscription topics at connector startup. Configure via `.with_topic_resolver()` on inbound connectors.
  - Topics resolved at connector startup via `collect_inbound_routes()` and per-message via `TopicProviderFn` for outbound

## [0.4.0] - 2025-12-25

### Fixed

- **MQTT Connector Deadlock with >10 Topics (Issue #63)**: Fixed initialization deadlock when subscribing to more than 10 MQTT topics. The fix has two parts:
  1. **Spawn-before-subscribe**: Event loop is now spawned before subscribing to topics, allowing continuous channel draining
  2. **Dynamic channel capacity**: Channel capacity now scales with topic count (`topics + 10`) instead of hard-coded `10`
  3. Added `tokio::task::yield_now().await` to ensure proper task scheduling before subscriptions

### Changed

- **Dependency Update**: Upgraded `rumqttc` from 0.24 to 0.25

## [0.3.0] - 2025-12-15

### Changed

- **Breaking: Record Registration**: Updated demo examples to use new key-based `configure<T>(key, |reg| ...)` API
- Demo records now have explicit keys (e.g., `"sensor.temp.indoor"`, `"sensor.temp.outdoor"`, `"command.temp.indoor"`)
- Demos refactored to use shared `mqtt-connector-demo-common` crate for cross-platform types and monitors

## [0.2.0] - 2025-11-20

### Added

- **Bidirectional MQTT Support**: Complete rewrite supporting simultaneous publishing and subscribing with automatic message routing
- **Inbound Message Routing**: Automatic routing of incoming MQTT messages to appropriate AimDB producers based on topic patterns
- **ConnectorBuilder Pattern**: New `MqttConnectorBuilder` for both Tokio and Embassy runtimes
- **Automatic Task Spawning**: Background tasks (connection management, message routing) now spawn automatically during `build()`
- **Router Integration**: Uses new `Router` system for type-safe message dispatch to correct record producers
- **Outbound Publisher Support**: Added `spawn_outbound_publishers()` method for both Tokio and Embassy implementations to handle AimDB → MQTT publishing via `ConsumerTrait`

### Changed

- **Breaking: Builder API**: Changed from `MqttConnector::new()` to `MqttConnectorBuilder::new()` with automatic initialization
- **Breaking: Task Management**: Removed manual `mqtt_background_task` spawning - tasks spawn automatically during database `build()`
- **Breaking: Configuration**: Simplified Embassy configuration with automatic network stack access
- **Breaking: Outbound Architecture**: Refactored to use `ConsumerTrait`-based outbound routing:
  - Added: `spawn_outbound_publishers()` method in both Tokio and Embassy implementations
  - Required: Must call `spawn_outbound_publishers()` in `ConnectorBuilder::build()` for outbound publishing to work
  - Changed: Outbound publishing now uses type-erased `ConsumerTrait` instead of automatic spawning
- **Tokio Client**: Refactored for bidirectional support with unified client and automatic reconnection
- **Embassy Client**: Simplified API with integrated task spawning and network stack management
- **Client ID**: Now properly passes client ID from user configuration instead of generating random IDs

### Fixed

- Session persistence with user-configured client IDs
- Reconnection handling in background tasks
- Topic subscription management for inbound routes

### Removed

- Manual background task spawning requirement in Embassy implementation
- Separate consumer registration API (now integrated into builder)

## [0.1.0] - 2025-11-06

### Added

- Initial release of MQTT connector for AimDB
- Dual runtime support for both Tokio and Embassy
- Automatic consumer registration via builder pattern
- Topic mapping with QoS and retain configuration
- Pluggable serializers (JSON, MessagePack, Postcard, custom)
- Automatic reconnection handling
- Uses `rumqttc` for std environments (Tokio)
- Uses `mountain-mqtt` for embedded environments (Embassy)
- Support for MQTT v3.1.1 protocol
- Configurable keep-alive and connection timeouts

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
