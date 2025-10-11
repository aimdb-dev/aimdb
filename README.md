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

> **⚠️ EARLY DEVELOPMENT STAGE**  
> AimDB is currently in **very early development**. The architecture is defined and the foundation is being built, but most features are not yet implemented. This is a work-in-progress project where core functionality is still being developed. Expect breaking changes and incomplete features.

> **One codebase. Any hardware. Always in sync.**

AimDB is an **async, in-memory database** that keeps state and streams **consistent across MCU → edge → cloud** — without internal brokers, glue code or vendor lock-in. If you've ever juggled MQTT bridges, SQLite caches and custom sync scripts just to move live data, AimDB is here to simplify your world.

---

## 🚀 Why AimDB Matters  
Modern devices generate massive real-time data, but today's stacks are fragmented and slow:  
- Multiple brokers/databases/sync layers to keep MCU, edge and cloud in step.  
- Device-specific integrations that make hardware swaps risky.  
- Batch-oriented pipelines that miss millisecond-level insights.  

AimDB collapses these layers into **one lightweight engine**:  
- Lock-free ring buffers + async transforms = **ms-level reactivity (<50 ms)**.  
- Protocol-agnostic bridges (MQTT, DDS, Kafka) = **no vendor lock**.  
- Portable across platforms with **swappable async runtimes**.  

---

## 🧩 High-Level Architecture & Tech Stack  
- **Language**: Rust 🦀 (async/await, `no_std` capable for MCUs)  
- **Runtime**: Supports **Embassy** for embedded async execution or standard Rust runtimes like Tokio/async-std.  
- **Data Core**: In-memory state + lock-free ring buffers + notifiers + async transforms.  
- **Protocols**: MQTT, Kafka, DDS (plug-in bridges).  
- **Platforms**: MCUs, Linux-class edge devices, cloud VMs/containers.  
- **Extras**: Built-in identity & access hooks, metering for future live-data assets.  

---

## 🏃 Quick Start  
Get up and running in **≤15 minutes** with our pre-configured development environment:

```bash
# 1. Clone the repo
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb

# 2. Open in VS Code with Dev Containers extension
code .
# Then: Ctrl/Cmd+Shift+P → "Dev Containers: Reopen in Container"

# 3. Inside the container, build and test:
make check  # Format, lint, and test

# 4. Run the examples:
cargo run --package tokio-runtime-demo          # Std runtime
cargo run --package embassy-runtime-demo        # Embedded (compile-only)
```

**✅ Zero Setup**: Rust, embedded targets and development tools pre-installed  
**✅ Cross-Platform**: Works on macOS, Linux, Windows (with Docker Desktop) or WSL
**✅ VS Code Ready**: Optimized extensions and settings included  

### Basic Usage

```rust
use aimdb_core::{Database, RecordT};
use aimdb_tokio_adapter::TokioAdapter;

#[derive(Debug, Clone)]
struct SensorData(String);
impl RecordT for SensorData {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build typed database
    let db = Database::<TokioAdapter>::builder()
        .record::<SensorData>(&Default::default())
        .build()?;
    
    // Produce data
    db.produce(SensorData("temp: 23.5°C".into())).await?;
    
    // Check stats
    let stats = db.producer_stats::<SensorData>();
    println!("Calls: {}", stats.call_count());
    
    Ok(())
}
```

> **💡 Architecture**: Type-safe records with `TypeId`-based routing. No string keys, no macros.

---

## 📦 Pluggable Buffers

AimDB provides three buffer types for per-record streaming, each optimized for different communication patterns:

### 1. **SPMC Ring Buffer** - High-Frequency Telemetry
```rust
use aimdb_tokio_adapter::TokioBuffer;
use aimdb_core::buffer::{BufferBackend, BufferCfg};

// Create bounded buffer with lag detection
let buffer = TokioBuffer::<SensorReading>::new(
    &BufferCfg::SpmcRing { capacity: 100 }
);

// Multiple consumers read independently
let reader1 = buffer.subscribe();
let reader2 = buffer.subscribe();

// Producers push data
buffer.push(SensorReading { temp: 23.5, humidity: 60.0 });

// Consumers handle lag gracefully
match reader1.recv().await {
    Ok(data) => process(data),
    Err(DbError::BufferLagged { lag_count, .. }) => {
        log::warn!("Skipped {} messages", lag_count);
    }
    _ => {}
}
```

**Use Cases**: 100+ Hz sensor streams, event monitoring, audit logs

### 2. **SingleLatest** - Configuration & State
```rust
// Only latest value matters - intermediate updates skipped
let buffer = TokioBuffer::<Config>::new(&BufferCfg::SingleLatest);

// Fast producer
for v in 1..=10 { buffer.push(Config { version: v }); }

// Slow consumer automatically skips to latest
let config = reader.recv().await?; // Gets v10, not v1-v9
```

**Use Cases**: Config updates, UI state sync, feature flags

### 3. **Mailbox** - Command Processing
```rust
// Single-slot with overwrite semantics
let buffer = TokioBuffer::<Command>::new(&BufferCfg::Mailbox);

// Rapid commands overwrite unread ones
buffer.push(Command::Start);
buffer.push(Command::Stop);  // Overwrites Start if not read yet

// Consumer gets latest command only
let cmd = reader.recv().await?;
```

**Use Cases**: Device control, actuator setpoints, RPC-style messaging

### Runtime Agnostic

**Same buffer API works on both Tokio (std) and Embassy (no_std)**:

```rust
// Tokio (cloud/edge)
use aimdb_tokio_adapter::TokioBuffer;
let buffer = TokioBuffer::<T>::new(&cfg);

// Embassy (MCU)
use aimdb_embassy_adapter::EmbassyBuffer;
type Buffer = EmbassyBuffer<T, 100, 4, 1, 1>;
let buffer = Buffer::new_spmc();
```

**📖 Full Guide**: See [Buffer Usage Guide](docs/design/003-M1_buffer_usage_guide.md) for choosing the right buffer type, performance tuning, and best practices.

---

## 🤝 Contributing  
We love contributions! Here's how to jump in:  
1. Clone the repository.
   ```bash
   git clone https://github.com/aimdb-dev/aimdb.git
   ```
2. Create a feature branch. 
   ```bash
   git checkout -b feature/my-awesome-idea
   ```
3. Follow our Coding Standards (Rustfmt + Clippy; clear commit messages).  
4. Open a Pull Request with a concise description and link any related issues.  
5. Discuss ideas or questions in GitHub Discussions or our chat (see below).  

Bug reports, docs fixes experimental connectors are all welcome — don't be shy!  

---

## 🛣 Roadmap & Help Wanted  
**Current Status**: Early development - foundational architecture is being implemented.

Core priorities where you can make a huge impact:  
- 🚧 **Core Database Engine** – implement in-memory storage with async operations  
- 🚧 **MCU Runtime** – tighten Embassy executor integration and notification system  
- 🧪 **Connectors** – expand MQTT/Kafka/DDS bridges, add gRPC/WebSocket bridges  
- 📊 **Observability** – lightweight metrics and health probes  
- 📚 **Docs & Examples** – more templates, edge-to-cloud demos  
- 🔐 **Access Control Hooks** – refine built-in identity & metering  

Check the Issues board for "help wanted" labels or propose your own ideas.  

---

## 🌐 Community  
- 💬 **Discussions**: [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions)
- 🐛 **Issues**: [Bug Reports & Feature Requests](https://github.com/aimdb-dev/aimdb/issues)
- 📖 **Docs**: [Project Wiki](https://github.com/aimdb-dev/aimdb/wiki)

Your voice shapes AimDB — ask questions, share feedback and showcase what you build.  

---

## ✨ Tagline  
**Let's build the future of edge intelligence — together!**
