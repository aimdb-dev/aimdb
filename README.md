<div align="center">
  <img src="assets/logo.png" alt="AimDB Logo" width="300">
</div>

[![Build Status](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/ci.yml?branch=main)](https://github.com/aimdb-dev/aimdb/actions)
[![Security Audit](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/security.yml?branch=main&label=security)](https://github.com/aimdb-dev/aimdb/actions)
[![Documentation](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/docs.yml?branch=main&label=docs)](https://github.com/aimdb-dev/aimdb/actions)
[![Crates.io](https://img.shields.io/crates/v/aimdb-core.svg)](https://crates.io/crates/aimdb-core)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)
[![Docs](https://docs.rs/aimdb-core/badge.svg)](https://docs.rs/aimdb-core)
[![Website](https://img.shields.io/badge/website-aimdb.dev-blue.svg)](https://aimdb.dev)

> **⚠️ PRE-RELEASE v0.4.0**  
> AimDB v0.4.0 introduces compile-time safe record keys with `#[derive(RecordKey)]`, multi-instance records, buffer metrics, MQTT/KNX connectors, and developer tools. The architecture is stable, but APIs may evolve based on community feedback. Production use is possible but proceed with caution and thorough testing.

> **One codebase. Any hardware. Always in sync.**

AimDB is an **async, in-memory database** for data synchronization across **MCU → edge → cloud** — without internal brokers or vendor lock-in. Built in Rust with `no_std` support for embedded systems.

---

## 🚀 Why AimDB?

Modern IoT stacks are fragmented:
- Multiple brokers/databases to sync MCU, edge, and cloud
- Device-specific integrations that make hardware swaps risky
- Batch-oriented pipelines that miss low-latency insights

**AimDB simplifies this:**
- **Fast**: Lock-free buffers + async transforms for <50ms reactivity
- **Portable**: Works on MCUs (Embassy), edge (Tokio), and cloud
- **Flexible**: Three buffer types (SPMC Ring, SingleLatest, Mailbox) for different patterns
- **Protocol-agnostic**: MQTT bridges ready, Kafka/DDS planned

---

## 🧩 Architecture

- **Language**: Rust 🦀 (async/await, `no_std` capable)
- **Runtimes**: Embassy (embedded) or Tokio (std)
- **Data Core**: Type-safe records with `TypeId` routing, three buffer types
- **Protocols**: MQTT ✅, KNX ✅, Kafka 🚧, DDS 🚧
- **Platforms**: MCUs, Linux edge devices, cloud VMs/containers

---

## 🎉 What's New in v0.4.0

**Latest Release** - December 25, 2025

Recent additions and improvements:

- 🆕 **Compile-Time Safe Keys**: New `#[derive(RecordKey)]` macro for type-safe record keys
- 🆕 **RecordKey Trait**: Enables user-defined enum keys with connector metadata
- ✅ **MQTT Deadlock Fix**: Fixed initialization issue with >10 MQTT topics (Issue #63)
- ✅ **Multi-Instance Records**: Register multiple records of the same type with unique keys
- ✅ **RecordId/RecordKey Architecture**: O(1) stable indexing with zero-allocation static keys
- ✅ **Buffer Metrics**: Comprehensive metrics for monitoring and debugging (feature-gated)
- ✅ **Enhanced Introspection**: New APIs for runtime record exploration
- ✅ **Type-Safe Core**: `TypeId`-based record routing eliminates runtime string lookups
- ✅ **Dual Runtime**: Works on both Tokio (std) and Embassy (no_std/embedded)
- ✅ **Three Buffer Types**: SPMC Ring, SingleLatest, and Mailbox patterns
- ✅ **MQTT Integration**: Connector works in both std and embedded environments
- ✅ **KNX Integration**: Building automation support for std and embedded
- ✅ **Remote Access**: AimX protocol for cross-process introspection
- ✅ **Sync API**: Blocking wrapper for non-async codebases
- ✅ **Developer Tools**: MCP server for LLM-powered debugging, CLI tools, and client library

See [CHANGELOG.md](CHANGELOG.md) for complete details.

---

## 📦 Installation

Add AimDB to your project:

```toml
# For standard library (Tokio runtime)
[dependencies]
aimdb-core = "0.3"
aimdb-tokio-adapter = "0.3"

# Optional: MQTT connector
aimdb-mqtt-connector = { version = "0.3", features = ["tokio-runtime"] }

# Optional: KNX connector (building automation)
aimdb-knx-connector = { version = "0.2", features = ["tokio-runtime"] }

# Optional: Synchronous API
aimdb-sync = "0.3"

# Optional: Remote client
aimdb-client = "0.3"

# Optional: Enable buffer metrics
# aimdb-core = { version = "0.3", features = ["metrics"] }
# aimdb-tokio-adapter = { version = "0.3", features = ["metrics"] }
```

For embedded systems using Embassy:

```toml
[dependencies]
aimdb-core = { version = "0.3", default-features = false }
aimdb-embassy-adapter = { version = "0.3", default-features = false, features = ["embassy-runtime"] }
aimdb-mqtt-connector = { version = "0.3", default-features = false, features = ["embassy-runtime"] }
aimdb-knx-connector = { version = "0.2", default-features = false, features = ["embassy-runtime"] }
```

---

## 🏃 Quick Start

### Option 1: Use the Dev Container (Recommended)

The fastest way to get started with a complete development environment:

```bash
# Clone the repository
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb

# Open in VS Code and reopen in container
code .  # Then: Dev Containers: Reopen in Container

# Build and test
make check

# Run an example
cargo run --example tokio-mqtt-connector-demo --features tokio-runtime,tracing
```

### Option 2: Local Development

**Prerequisites**: Rust 1.75+ (2021 edition)

```bash
# Clone and build
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
cargo build --all-features

# Run tests
make test

# Generate documentation
make doc
```

### Basic Usage

```rust
use aimdb_core::{AimDbBuilder, DbResult, Producer, RuntimeContext};
use aimdb_core::buffer::BufferCfg;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Debug, Clone)]
struct Temperature { celsius: f32 }

// Producer: generates temperature readings
async fn temperature_producer(
    ctx: RuntimeContext<TokioAdapter>,
    producer: Producer<Temperature, TokioAdapter>,
) {
    let temp = Temperature { celsius: 23.5 };
    producer.produce(temp).await.ok();
}

#[tokio::main]
async fn main() -> DbResult<()> {
    let runtime = Arc::new(TokioAdapter::new()?);
    
    let mut builder = AimDbBuilder::new().runtime(runtime);
    
    builder.configure::<Temperature>("sensor.temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 32 })
           .source(temperature_producer);
    });
    
    builder.run().await
}
```

For complete examples with consumers and MQTT integration, see the `/examples` directory.

---

## 📦 Buffer Types

Choose the right buffer for your data pattern:

**1. SPMC Ring** - High-frequency telemetry (100+ Hz sensors, logs)
```rust
reg.buffer_sized::<100>(BufferType::SpmcRing);
```
Multiple consumers read independently. Handles lag with explicit notifications.

**2. SingleLatest** - Configuration & state (UI sync, feature flags)
```rust
reg.buffer_sized::<10>(BufferType::SingleLatest);
```
Only newest value matters. Consumers skip intermediate updates automatically.

**3. Mailbox** - Commands & control (device control, RPC)
```rust
reg.buffer_sized::<1>(BufferType::Mailbox);
```
Single slot with overwrite. Latest command wins.

**Runtime Agnostic**: Same API works on Tokio (std) and Embassy (no_std).

---

## 🚧 Roadmap

**✅ Completed:**
- Core database with type-safe records
- Tokio & Embassy runtime adapters
- Three buffer types with simplified API
- MQTT connector (std and embedded)
- KNX connector (std and embedded) - building automation
- MCP server for LLM-powered introspection
- CLI tools (basic implementation)
- Remote access protocol (AimX v1)
- Client library for remote database access
- Synchronous API wrapper
- Comprehensive CI/CD and security auditing

**🔨 In Progress:**
- Performance benchmarks and optimization
- HTTP/REST bridge


**📋 Planned:**
- Kafka connector (std environments)
- DDS connector for low-latency systems
- Advanced observability and metrics
- Multi-instance clustering

---

## 🤝 Contributing

We welcome contributions! AimDB is open source and community-driven.

**Ways to contribute:**
- 🐛 Report bugs and request features via [GitHub Issues](https://github.com/aimdb-dev/aimdb/issues)
- 💡 Join discussions on design and architecture
- 📝 Improve documentation and examples
- 🔧 Submit pull requests with bug fixes or features
- ⭐ Star the repo to show your support!

**Getting Started:**
1. Fork the repository
2. Create a feature branch: `git checkout -b feature/my-idea`
3. Make your changes and add tests
4. Run `make check` to validate (fmt + clippy + tests)
5. Submit a PR with a clear description

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines, coding standards, and development workflow.

---

## 🌟 Community & Support

- **Issues**: Report bugs or request features at [GitHub Issues](https://github.com/aimdb-dev/aimdb/issues)
- **Discussions**: Join the conversation in [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions)
- **Documentation**: Full API docs at [docs.rs/aimdb](https://docs.rs/aimdb)
- **Examples**: Working demos in the `/examples` directory

---

## 📚 Documentation

- **Changelog**: See [CHANGELOG.md](CHANGELOG.md) for release history
- **Examples**: Check `/examples` for working demos:
  - `tokio-mqtt-connector-demo` - Full MQTT integration with Tokio
  - `embassy-mqtt-connector-demo` - Embedded MQTT on RP2040
  - `tokio-knx-connector-demo` - KNX building automation with Tokio
  - `embassy-knx-connector-demo` - Embedded KNX on microcontroller
  - `sync-api-demo` - Synchronous API wrapper usage
  - `remote-access-demo` - Cross-process introspection server
- **Design Docs**: See `/docs/design` for architecture details
- **API Docs**: Run `make doc` to generate rustdoc
- **Contributing**: Read [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines

---

## � License

Licensed under Apache License 2.0. See [LICENSE](LICENSE) for details.

---

**Let's build the future of edge intelligence — together!**
