<div align="center">
  <img src="assets/logo.png" alt="AimDB Logo" width="300">
</div>

[![Build Status](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/ci.yml?branch=main)](https://github.com/aimdb-dev/aimdb/actions)
[![Security Audit](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/security.yml?branch=main&label=security)](https://github.com/aimdb-dev/aimdb/actions)
[![Documentation](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/docs.yml?branch=main&label=docs)](https://github.com/aimdb-dev/aimdb/actions)
[![Crates.io](https://img.shields.io/crates/v/aimdb.svg)](https://crates.io/crates/aimdb)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)
[![Docs](https://docs.rs/aimdb/badge.svg)](https://docs.rs/aimdb)
[![Website](https://img.shields.io/badge/website-aimdb.dev-blue.svg)](https://aimdb.dev)

> **⚠️ ACTIVE DEVELOPMENT**  
> AimDB is in active development. Core architecture is functional with working runtime adapters (Tokio & Embassy), buffer systems, and MQTT connectivity. API may change as we refine the design.

> **One codebase. Any hardware. Always in sync.**

AimDB is an **async, in-memory database** for real-time data synchronization across **MCU → edge → cloud** — without internal brokers or vendor lock-in. Built in Rust with `no_std` support for embedded systems.

---

## 🚀 Why AimDB?

Modern IoT stacks are fragmented:
- Multiple brokers/databases to sync MCU, edge, and cloud
- Device-specific integrations that make hardware swaps risky
- Batch-oriented pipelines that miss real-time insights

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
- **Protocols**: MQTT ✅, Kafka 🚧, DDS 🚧
- **Platforms**: MCUs, Linux edge devices, cloud VMs/containers

---

## 🏃 Quick Start

**Prerequisites**: Docker (for dev container) or Rust 1.75+

```bash
# Clone and open in VS Code dev container
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
code .  # Then: Dev Containers: Reopen in Container

# Build and test
make check

# Run examples
cargo run --example tokio-mqtt-connector-demo
```

### Basic Usage

```rust
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_tokio_adapter::TokioAdapter;

#[derive(Debug, Clone)]
struct Temperature { celsius: f32 }

#[tokio::main]
async fn main() -> DbResult<()> {
    let runtime = Arc::new(TokioAdapter::new());
    
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .with_record::<Temperature>(|reg| {
            reg.buffer_sized::<32>(TokioBufferType::SpmcRing);
        })
        .build()?;
    
    // Produce data
    db.produce(Temperature { celsius: 23.5 }).await?;
    
    // Subscribe to updates
    let mut reader = db.subscribe::<Temperature>()?;
    if let Ok(temp) = reader.recv().await {
        println!("Temperature: {:.1}°C", temp.celsius);
    }
    
    Ok(())
}
```

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
- Extension trait macro system

**� In Progress:**
- Kafka connector (std environments)
- CLI tools for debugging and introspection
- Performance benchmarks

**📋 Planned:**
- DDS connector for real-time systems
- HTTP/REST bridge
- Advanced observability

---

## 🤝 Contributing

Contributions welcome! Here's how:

1. Clone the repository
2. Create a feature branch: `git checkout -b feature/my-idea`
3. Run `make check` to validate (fmt + clippy + tests)
4. Submit a PR with a clear description

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines.

---

## � Documentation

- **Examples**: Check `/examples` for working demos
- **Design Docs**: See `/docs/design` for architecture details
- **API Docs**: Run `make doc` to generate rustdoc

---

## � License

Licensed under Apache License 2.0. See [LICENSE](LICENSE) for details.

---

**Let's build the future of edge intelligence — together!**
