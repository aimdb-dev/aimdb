# AimDB v0.1.0 Release Notes

**The first stable release of AimDB - async, in-memory database for data synchronization across MCU → edge → cloud environments.**

---

## 🎯 What is AimDB?

AimDB eliminates the complexity of syncing data across embedded devices, edge servers, and cloud infrastructure. Write once, run anywhere - from a 32-bit ARM microcontroller to a cloud VM.

**Key Innovation:** One codebase, dual runtime support (Tokio for std, Embassy for no_std embedded), with type-safe records and lock-free buffers targeting <50ms latency.

---

## ✨ Highlights

### 🔄 Dual Runtime Support
- **Tokio Adapter**: Full-featured for standard library environments
- **Embassy Adapter**: Optimized for embedded no_std targets (ARM Cortex-M)
- **Same API**: Write code once, compile for any platform

### 🔒 Type-Safe Architecture
- `TypeId`-based record routing (no string lookups)
- Compile-time type checking
- Zero-cost abstractions

### 📦 Three Buffer Strategies
Choose the right pattern for your data:
- **SPMC Ring**: High-frequency streams (sensor telemetry, logs)
- **SingleLatest**: State sync (configuration, UI state)
- **Mailbox**: Commands and control (device control, RPC)

### 🔌 MQTT Connector
- Works in both std and no_std environments
- Automatic topic mapping
- Custom serialization support (JSON, MessagePack, Postcard)
- QoS and retain configuration

### 🤖 MCP Server (Bonus!)
- LLM-powered introspection and debugging
- Query schemas, subscribe to updates, set values
- JSON Schema inference from runtime values
- Perfect for AI-assisted development

---

## 📦 Published Crates

All crates are now available on crates.io:

| Crate | Description | Features |
|-------|-------------|----------|
| `aimdb-executor` | Runtime trait abstractions | Tokio, Embassy support |
| `aimdb-core` | Core database engine | std, no_std, tracing, metrics |
| `aimdb-tokio-adapter` | Tokio runtime implementation | Full async/await support |
| `aimdb-embassy-adapter` | Embassy runtime for embedded | Configurable task pools |
| `aimdb-client` | AimX protocol client library | Remote introspection |
| `aimdb-sync` | Synchronous wrapper API | Blocking operations |
| `aimdb-mqtt-connector` | MQTT integration | Dual runtime support |
| `aimdb-cli` | Command-line interface | Discovery, inspection |
| `aimdb-mcp` | Model Context Protocol server | LLM integration |

---

## 🚀 Quick Start

### Standard Library (Tokio)

```toml
[dependencies]
aimdb-core = "0.1"
aimdb-tokio-adapter = "0.1"
```

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::TokioAdapter;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct Temperature {
    celsius: f32,
    sensor_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .with_record::<Temperature>(|reg| {
            reg.buffer(BufferCfg::SingleLatest);
        })
        .build()?;
    
    // Produce data
    db.produce(Temperature { 
        celsius: 23.5, 
        sensor_id: "sensor-001".to_string() 
    }).await?;
    
    // Subscribe to updates
    let mut reader = db.subscribe::<Temperature>()?;
    if let Ok(temp) = reader.recv().await {
        println!("Temperature: {:.1}°C from {}", temp.celsius, temp.sensor_id);
    }
    
    Ok(())
}
```

### Embedded (Embassy)

```toml
[dependencies]
aimdb-core = { version = "0.1", default-features = false }
aimdb-embassy-adapter = { version = "0.1", features = ["embassy-runtime"] }
```

```rust
#![no_std]
#![no_main]

use aimdb_core::AimDbBuilder;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType};
use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let runtime = Arc::new(EmbassyAdapter::new_with_spawner(spawner));
    
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .with_record::<SensorData>(|reg| {
            reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing);
        })
        .build()
        .unwrap();
    
    // Use the database...
}
```

---

## 📚 Documentation

- **Main README**: [README.md](https://github.com/aimdb-dev/aimdb/blob/main/README.md)
- **CHANGELOG**: [CHANGELOG.md](https://github.com/aimdb-dev/aimdb/blob/main/CHANGELOG.md)
- **Contributing Guide**: [CONTRIBUTING.md](https://github.com/aimdb-dev/aimdb/blob/main/CONTRIBUTING.md)
- **API Docs**: [docs.rs/aimdb](https://docs.rs/aimdb) (coming soon)
- **Design Docs**: [/docs/design](https://github.com/aimdb-dev/aimdb/tree/main/docs/design)

---

## 📖 Examples

Complete working examples included:

1. **tokio-mqtt-connector-demo**: Full MQTT integration with Tokio
2. **embassy-mqtt-connector-demo**: Embedded MQTT on RP2040 WiFi
3. **sync-api-demo**: Synchronous API usage patterns
4. **remote-access-demo**: Cross-process introspection server

```bash
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
cargo run --example tokio-mqtt-connector-demo
```

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────┐
│      Your Application (Tokio or Embassy)       │
└─────────────────────┬───────────────────────────┘
                      │
         ┌────────────┴────────────┐
         ▼                         ▼
   ┌──────────┐            ┌──────────────┐
   │  AimDB   │            │  Connectors  │
   │  Core    │◄───────────┤  MQTT/Kafka  │
   └────┬─────┘            └──────────────┘
        │
   ┌────┴────────────────────┐
   ▼                         ▼
Producer                 Consumer
Tasks                    Tasks
```

---

## ⚡ Performance

- **Target**: <50ms end-to-end latency
- **Lock-free**: Buffer operations use atomic primitives
- **Zero-copy**: Where possible
- **Minimal allocations**: Optimized hot paths

Benchmarks coming in v0.2.0!

---

## 🛣️ Roadmap

### v0.2.0 (Next)
- DDS/Kafka connectors for std environments
- Performance benchmarks and profiling
- Enhanced CLI with more commands
- Additional buffer statistics and metrics

### Future
- HTTP/REST bridge
- Advanced observability
- GUI debugging tools

---

## 🤝 Contributing

We welcome contributions! See [CONTRIBUTING.md](https://github.com/aimdb-dev/aimdb/blob/main/CONTRIBUTING.md) for:

- Development setup
- Code standards
- Testing guidelines
- License compliance

**Quick start:**
```bash
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
make check  # Run all quality checks
```

---

## 📄 License

Licensed under Apache License 2.0 - see [LICENSE](https://github.com/aimdb-dev/aimdb/blob/main/LICENSE) for details.

All dependencies use permissive licenses compatible with commercial use (MIT, Apache-2.0, BSD variants).

---

## 🙏 Acknowledgments

Built with:
- **Tokio**: Async runtime for Rust
- **Embassy**: Embedded async framework
- **rumqttc** & **mountain-mqtt**: MQTT client libraries
- **Embassy team**: For excellent embedded async support

Special thanks to everyone who provided feedback during development!

---

## 💬 Community

- **Issues**: [GitHub Issues](https://github.com/aimdb-dev/aimdb/issues)
- **Discussions**: [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions)
- **Website**: https://aimdb.dev (coming soon)

---

## 🎉 Get Started Today!

```bash
cargo add aimdb-core aimdb-tokio-adapter
```

**Let's build the future of edge intelligence — together!**
