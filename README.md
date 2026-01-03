<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo.png">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo.png">
    <img src="assets/logo.png" alt="AimDB" width="450" style="background-color: white; padding: 20px; border-radius: 12px;">
  </picture>
</p>
<p align="center">
    <em>Move compute to your data. Not data to your cloud.</em>
</p>
<p align="center">
<a href="https://github.com/aimdb-dev/aimdb/stargazers/" target="_blank">
    <img src="https://img.shields.io/github/stars/aimdb-dev/aimdb?style=social&label=Star&maxAge=2592000" alt="Stars">
</a>
<a href="https://github.com/aimdb-dev/aimdb/releases" target="_blank">
    <img src="https://img.shields.io/github/v/release/aimdb-dev/aimdb?color=white" alt="Release">
</a>
<a href="https://crates.io/crates/aimdb-core" target="_blank">
    <img src="https://img.shields.io/crates/v/aimdb-core.svg" alt="Crates.io">
</a>
<a href="https://github.com/aimdb-dev/aimdb/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/ci.yml?branch=main" alt="Build">
</a>
<a href="LICENSE" target="_blank">
    <img src="https://img.shields.io/badge/license-Apache%202.0-blue.svg" alt="License">
</a>
</p>

Cloud costs are exploding. Compute and storage bills grow with every byte shipped upstream. But refactoring to run at the edge means rewriting everything.

AimDB solves this with **portable data contracts**: define your schemas, serialization and transforms once — deploy them anywhere. The same code runs on MCUs, edge gateways and Kubernetes. Move processing closer to the source when costs spike or keep it in the cloud when you need scale. Your choice.

<p align="center">
  <img src="assets/architecture.svg" alt="AimDB Architecture" width="700">
</p>

---

### Getting Started

- [Quick Start Guide](docs/aimdb-usage-guide.md) — Get running in 5 minutes
- [Examples](examples/) — MQTT, KNX and remote access demos
- [API Documentation](https://docs.rs/aimdb-core) — Full Rust API reference

**Linux / Cloud (Tokio)**
```toml
[dependencies]
aimdb-core = { version = "0.4", features = ["std"] }
aimdb-tokio-adapter = { version = "0.4", features = ["tokio-runtime"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

**Embedded MCUs (Embassy)**
```toml
[dependencies]
aimdb-core = { version = "0.4", default-features = false }
aimdb-embassy-adapter = { version = "0.4", default-features = false, features = [
    "embassy-runtime",
    "embassy-task-pool-8",  # 8, 16 or 32 based on task count
] }
serde = { version = "1", default-features = false, features = ["derive"] }
```

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[derive(Clone)]
struct Temperature { celsius: f32 }

#[tokio::main]
async fn main() -> aimdb_core::DbResult<()> {
    let runtime = std::sync::Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(runtime);
    
    builder.configure::<Temperature>("sensor.temp", |reg| {
        reg.buffer(BufferCfg::SingleLatest);  // Only keep latest
    });
    
    builder.run().await
}
```

---

### Why AimDB?

| Problem | AimDB Solution |
|---------|----------------|
| **Cloud costs spiking** | Move processing to edge — same code, no rewrite |
| **Edge-only is inflexible** | Run anywhere: MCU, gateway or cloud |
| **Vendor lock-in** | Open source, protocol-agnostic |
| **Fragmented tooling** | One codebase, portable schemas |
| **Latency** | <50ms reactivity when running local |

---

### Connectors

| Protocol | Status | Use Case |
|----------|--------|----------|
| **MQTT** | ✅ Ready | IoT messaging, telemetry |
| **KNX** | ✅ Ready | Building automation |
| **HTTP/REST** | 🔨 Building | Web APIs, webhooks |
| **Kafka** | 📋 Planned | Event streaming |
| **Modbus** | 📋 Planned | Industrial automation |
| **OPC-UA** | 📋 Planned | Manufacturing systems |

---

### Platform Support

| Target | Runtime | Status |
|--------|---------|--------|
| **MCUs** (ARM Cortex-M) | Embassy | ✅ `no_std` ready |
| **Edge** (Linux/RPi) | Tokio | ✅ Full featured |
| **Cloud** (Containers) | Tokio | ✅ Full featured |

---

### Contributing

Found a bug or want a feature? Open a [GitHub issue](https://github.com/aimdb-dev/aimdb/issues). 

Want to contribute? See the [contributing guide](CONTRIBUTING.md). We have [good first issues](https://github.com/aimdb-dev/aimdb/labels/good-first-issue) to get started.

---

### License

[Apache 2.0](LICENSE)

---

<p align="center">
  <strong>Write once. Deploy anywhere. Pay only where it makes sense.</strong>
</p>
