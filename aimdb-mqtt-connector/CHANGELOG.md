# Changelog - aimdb-mqtt-connector

All notable changes to the `aimdb-mqtt-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- MQTT Connector Deadlock with >10 Topics**: Fixed initialization deadlock when subscribing to more than 10 MQTT topics. The fix has two parts:
  1. **Spawn-before-subscribe**: Event loop is now spawned before subscribing to topics, allowing continuous channel draining
  2. **Dynamic channel capacity**: Channel capacity now scales with topic count (`topics + 10`) instead of hard-coded `10`

### Changed

- **Dependency Update**: Upgraded `rumqttc` from 0.24 to 0.25
- **Breaking: Record Registration**: Updated demo examples to use new key-based `configure<T>(key, |reg| ...)` API
- Demo records now have explicit keys (e.g., `"sensor.temperature"`, `"command.temperature"`, `"sensors.temperature"`, `"commands.temperature"`)

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
