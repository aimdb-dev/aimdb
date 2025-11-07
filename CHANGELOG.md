# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
