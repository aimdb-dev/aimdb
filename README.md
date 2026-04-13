<p align="center">
  <img src="assets/aimdb-logo.svg" alt="AimDB" width="450">
</p>
<p align="center">
    <strong>Distributed by design. Data-driven by default.</strong>
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

AimDB turns data contracts into the architecture. Define your schemas once — with built-in versioning, observability and serialization — and deploy them unchanged across microcontrollers, edge gateways, Kubernetes and the browser.

---

### Vision

A future where every system — from a $2 sensor to a global fleet — shares one data language. Contracts define how data moves, evolves and is observed. Infrastructure adapts to the data, not the other way around.

---

### Design Philosophy

In a data-driven architecture, every design decision starts with the data, not the service that produces it.

**Records declare their own semantics.** When you register a record in AimDB, you choose a buffer type that defines how the data moves:

| Buffer | Semantics | Use Cases |
|--------|-----------|-----------|
| **SPMC Ring** | Bounded stream with independent consumers | Sensor telemetry, event logs |
| **SingleLatest** | Only the current value matters | Feature flags, configuration, UI state |
| **Mailbox** | Latest instruction wins | Device commands, actuation, RPC |

These are the three universal primitives of data movement — portable, typed and runtime-agnostic.

**Observability becomes automatic.** A record that exists is observable by definition. Every producer and consumer relationship is declared in the builder, not discovered through instrumentation.

**Synchronization becomes declarative.** You don't build a sync layer between your MCU, edge gateway and cloud backend. You declare a record with connector metadata on its key and the same typed data flows across all environments without translation.

**Cross-cutting concerns derive from the schema.** Instead of adding observability libraries, feature flag SDKs and experiment frameworks as separate integrations, they become intrinsic properties of records — declared once, applied everywhere.

---

### How It Works

Define your contracts, choose buffer semantics and wire up connectors — all in one builder block:

```rust
// A sensor node: produce temperature readings, publish over MQTT
builder.configure::<Temperature>("sensor::temp", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 256 })
        .source(|ctx, producer| async move {
            loop {
                let reading = read_sensor().await;
                producer.send(Temperature::set(reading, now())).await.ok();
                ctx.sleep(Duration::from_secs(1)).await;
            }
        })
        .link_to("mqtt://sensors/temperature")
        .with_serializer(Temperature::to_bytes)
        .finish();
});

// An edge gateway: receive from MQTT, observe and forward
builder.configure::<Temperature>("gateway::temp", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .link_from("mqtt://sensors/temperature")
        .with_deserializer(Temperature::from_bytes)
        .tap(log_tap::<Temperature>("edge"))   // [edge] 22.5 °C
        .finish();
});
```

Transport topics can be passed as strings to `link_to` / `link_from`, or declared on key enums with `#[link_address = "mqtt://..."]` and resolved at runtime. No separate instrumentation. No SDK integration. No sync code.

---

### Data Contracts

Data contracts are the heart of AimDB. A contract is a plain Rust struct that carries its own identity, version and capabilities — the single source of truth from sensor firmware to browser UI.

```rust
use aimdb_data_contracts::{SchemaType, Settable};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Temperature {
    pub celsius: f32,
    pub timestamp: u64,
}

impl SchemaType for Temperature {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 1;
}

impl Settable for Temperature {
    type Value = f32;
    fn set(value: Self::Value, timestamp: u64) -> Self {
        Self { celsius: value, timestamp }
    }
}
```

This struct compiles for `no_std` embedded targets and standard Rust alike. `SchemaType` gives the record its identity and version. `Settable` provides a canonical constructor so producers can create records from a raw value — this is the interface used by `producer.send(Temperature::set(reading, now()))` in the builder.

#### Contract Attributes

Contracts gain capabilities through trait implementations. Each trait is a compile-time declaration of what a contract *can do*, not a runtime configuration:

| Attribute | Trait | What It Enables |
|-----------|-------|-----------------|
| **Settable** | `Settable` | Canonical constructor from a raw value — the interface behind `producer.send(T::set(value, ts))`. |
| **Streamable** | `Streamable` | Cross-boundary transport — WASM, WebSocket, CLI. One registry, zero parallel type systems. |
| **Migratable** | `MigrationStep` | Bidirectional schema evolution with typed up/down transforms and chained version steps. |
| **Observable** | `Observable` | Signal extraction for thresholds, logging and monitoring. Icon, unit and `format_log()` built in. |
| **Linkable** | `Linkable` | Wire-format serialization for connectors — MQTT, KNX and any future transport. |
| **Simulatable** | `Simulatable` | Realistic test data generation with random walks, trends and configurable parameters. |

For example, `Observable` turns a contract into a loggable, monitorable signal:

```rust
impl Observable for Temperature {
    type Signal = f32;
    const ICON: &'static str = "thermometer";
    const UNIT: &'static str = "°C";

    fn signal(&self) -> f32 { self.celsius }

    fn format_log(&self, node_id: &str) -> String {
        format!("[{}] {:.1} °C", node_id, self.celsius)
    }
}
```

Each trait you implement unlocks a capability — contracts without `Observable` simply can't be tapped; contracts without `Linkable` can't be wired to connectors. The type system enforces what your data can do.

#### Platform-Agnostic by Design

The same contract works across all runtimes without modification:

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                           Temperature Contract                                │
├───────────────────┬───────────────────┬───────────────────┬───────────────────┤
│  MCU (Embassy)    │  Edge (Tokio)     │  Cloud (Tokio)    │  Browser (WASM)   │
│  no_std + alloc   │  std              │  Kubernetes       │  wasm32           │
│  Cortex-M4        │  Linux / RPi      │  Full featured    │  Single-threaded  │
└───────────────────┴───────────────────┴───────────────────┴───────────────────┘
```

The Rust type system enforces correctness at compile time. The dataflow engine's buffer semantics enforce flow guarantees at runtime. Connectors wire everything to your infrastructure without an integration layer.

---

### Getting Started

#### 1. See it live

Explore a running sensor mesh — no setup required:

<p align="center">
  <a href="https://aimdb.dev">
    <img src="assets/demo.gif" alt="AimDB Live Demo" width="600">
  </a>
</p>

> **[aimdb.dev](https://aimdb.dev)** — live weather stations streaming typed contracts across MCU, edge and cloud.

#### 2. Explore with AI

Connect an MCP-compatible editor to the live demo and query your data in natural language — no install required:

<p align="center">
  <img src="assets/copilot-communication.gif" alt="AimDB MCP Live Demo" width="600">
</p>

Add the remote MCP server to your workspace:

`.vscode/mcp.json`:

```json
{
  "servers": {
    "aimdb-weather": {
      "type": "http",
      "url": "http://aimdb.dev/mcp"
    }
  }
}
```

Then ask: *"What's the current temperature in Munich?"* — see the [MCP server docs](tools/aimdb-mcp/) for Claude Desktop and other editors.

#### 3. Run locally

Spin up a full MCU → edge → cloud mesh with one command:

```bash
cd examples/weather-mesh-demo
docker compose up
```

This starts three weather stations, an MQTT broker and a central hub — all wired together with typed data contracts.

#### 4. Build your own

- [Quick Start Guide](https://aimdb.dev/docs/getting-started) — Dependencies, platform setup and your first contract
- [Data Contracts](https://aimdb.dev/docs/data-contracts) — Type-safe schemas with built-in capabilities
- [Connectors](https://aimdb.dev/docs/connectors) — MQTT, KNX, WebSocket and more
- [Deployment](https://aimdb.dev/docs/deployment) — Running on MCU, edge, cloud and browser
- [API Reference](https://docs.rs/aimdb-core) — Full Rust API documentation
- [Blog](https://aimdb.dev/blog) — News, tutorials and insights from the AimDB team

---

### Connectors

| Protocol | Crate | Status | Runtimes |
|----------|-------|--------|----------|
| **MQTT** | `aimdb-mqtt-connector` | ✅ Ready | std, no_std |
| **KNX** | `aimdb-knx-connector` | ✅ Ready | std, no_std |
| **WebSocket** | `aimdb-websocket-connector` | ✅ Ready | std, wasm |
| **Kafka** | — | 📋 Planned | std |
| **Modbus** | — | 📋 Planned | std, no_std |

---

### Platform Support

| Target | Runtime | Adapter | Features | Footprint |
|--------|---------|---------|----------|-----------|
| **ARM Cortex-M** (STM32H5, STM32F4) | Embassy | `aimdb-embassy-adapter` | no_std, async | ~50KB+ |
| **Linux Edge** (RPi, gateways) | Tokio | `aimdb-tokio-adapter` | Full std | ~10MB+ |
| **Containers / K8s** | Tokio | `aimdb-tokio-adapter` | Full std | ~10MB+ |
| **Browser / SPA** | WASM | `aimdb-wasm-adapter` | wasm32, single-threaded | ~2MB+ |

---

### Contributing

Found a bug or want a feature? Open a [GitHub issue](https://github.com/aimdb-dev/aimdb/issues).

Have questions or ideas? Join the discussion on [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions).

Want to contribute? See the [contributing guide](CONTRIBUTING.md). We have [good first issues](https://github.com/aimdb-dev/aimdb/labels/good-first-issue) to get started.

---

### License

[Apache 2.0](LICENSE)

---

<p align="center">
  <strong>Define once. Deploy anywhere. Observe everything.</strong>
  <br><br>
  <a href="https://aimdb.dev/docs/getting-started">Get started</a> · <a href="https://aimdb.dev">Live demo</a> · <a href="https://github.com/aimdb-dev/aimdb/discussions">Join the discussion</a>
</p>
