# AimDB v0.2.0 Release Notes

**Major update with bidirectional connectors and KNX/IP integration!**

---

## 🎯 What's New in v0.2.0

This release transforms AimDB connectors from read-only consumers to **full bidirectional bridges**, enabling read AND write operations. Plus, we're adding KNX/IP support for building automation!

---

## ✨ Major Features

### 🔄 Bidirectional Connectors
Connectors can now **produce data INTO AimDB**, not just consume from it!

**Key Change:** Configure connectors at the database level using `.with_connector()` and route configuration happens during record setup with `.link_to()` (outbound) and `.link_from()` (inbound).

**Before (v0.1.0):**
```rust
// Limited API - separate connector setup
let connector = MqttConnector::new(runtime, db, client);
```

**Now (v0.2.0):**
```rust
// Integrated connector configuration in database builder
let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(
        MqttConnector::new("mqtt://broker:1883")
            .with_client_id("my-client")
    );

// Bidirectional routing per record type
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
        // Outbound: AimDB → MQTT
        .link_to("mqtt://sensors/temperature")
        .with_serializer(|temp: &Temperature| {
            serde_json::to_vec(temp)
                .map_err(|_| SerializeError::InvalidData)
        })
        .finish();
});

builder.configure::<Command>(|reg| {
    reg.buffer(BufferCfg::Mailbox)
        // Inbound: MQTT → AimDB
        .link_from("mqtt://commands/device")
        .with_deserializer(|data: &[u8]| {
            serde_json::from_slice(data)
                .map_err(|e| format!("Parse error: {}", e))
        })
        .finish();
});

builder.run().await
```

### 🏠 KNX/IP Connector (Preview)
Integration with KNX building automation systems!

```rust
use aimdb_knx_connector::KnxConnector;

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(
        KnxConnector::new("knx://192.168.1.1:3671")
            .with_client_id("aimdb-knx-001")
    );

// Configure bidirectional KNX routes
builder.configure::<LightState>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
        // Subscribe to KNX group address
        .link_from("knx://1/2/3")
        .with_deserializer(parse_light_state)
        .finish()
        // Write to KNX group address
        .link_to("knx://1/2/3")
        .with_serializer(serialize_light_state)
        .finish();
});

builder.run().await?;
```

**Note:** KNX connector uses git dependencies until `knx-pico` is published to crates.io:
```toml
aimdb-knx-connector = { git = "https://github.com/aimdb-dev/aimdb", tag = "v0.2.0" }
```

### ⚡ Integrated Builder Pattern
Connectors are now configured directly in the database builder:

```rust
// Old (v0.1.0)
let db = AimDbBuilder::new().runtime(runtime).build()?;
let connector = MqttConnector::new(runtime, db, client);

// New (v0.2.0) - Integrated configuration
let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(
        MqttConnector::new("mqtt://broker:1883")
            .with_client_id("client-id")
    );

// Configure records with routing
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .link_to("mqtt://sensors/temperature")
        .with_serializer(serde_json::to_vec)
        .finish();
});

builder.run().await?;  // Start database with connector
```

### 🔧 Enhanced Sync API
Better error handling and clearer semantics:

```rust
use aimdb_sync::{SyncAimDb, SyncProducer, SyncConsumer};

// Builder pattern with explicit Send + Sync trait bounds
let sync_db = SyncAimDb::new(db)?;
let producer = sync_db.producer::<MyRecord>()?;
let consumer = sync_db.consumer::<MyRecord>()?;
```

---

## 🔨 Breaking Changes

### Connector API Changes
- Connectors configured via `AimDbBuilder::with_connector()` instead of separate instantiation
- Routing configured per-record with `.link_to()` / `.link_from()` instead of global route lists
- Database starts with `builder.run().await` instead of `builder.build()`
- Added serializer/deserializer per route for type-safe conversion

**Migration:**
```rust
// Before (v0.1.0)
let db = AimDbBuilder::new().runtime(runtime).build()?;
let connector = MqttConnector::new(runtime, db, mqtt_client);

// After (v0.2.0)
let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(
        MqttConnector::new("mqtt://broker:1883")
            .with_client_id("client-id")
    );

builder.configure::<MyRecord>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .link_to("mqtt://my/topic")
        .with_serializer(|rec| serde_json::to_vec(rec).map_err(|_| SerializeError::InvalidData))
        .finish();
});

builder.run().await?;  // ← Changed from build()
```

### Sync API Changes
- Renamed `AimDbSync` → `SyncAimDb`
- Renamed `produce_sync()` → `produce()`
- Added explicit `Send + Sync` trait bounds

**Migration:**
```rust
// Before
use aimdb_sync::AimDbSync;
let sync_db = AimDbSync::new(db)?;

// After
use aimdb_sync::SyncAimDb;
let sync_db = SyncAimDb::new(db)?;
```

---

## 📦 Published Crates

### Updated to v0.2.0
- ✅ `aimdb-core` - Core database engine
- ✅ `aimdb-tokio-adapter` - Tokio runtime adapter
- ✅ `aimdb-embassy-adapter` - Embassy runtime adapter
- ✅ `aimdb-client` - AimX protocol client
- ✅ `aimdb-sync` - Synchronous wrapper API
- ✅ `aimdb-mqtt-connector` - MQTT integration
- ✅ `aimdb-cli` - Command-line interface
- ✅ `aimdb-mcp` - Model Context Protocol server

### Unchanged
- `aimdb-executor` - Still at v0.1.0 (no changes)

### Not Published (Yet)
- `aimdb-knx-connector` - Use git dependency until knx-pico is on crates.io

---

## 🚀 Quick Start

### Bidirectional MQTT Example

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_mqtt_connector::MqttConnector;
use serde::{Serialize, Deserialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temperature {
    celsius: f32,
    sensor_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Command {
    action: String,
    target: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(
            MqttConnector::new("mqtt://localhost:1883")
                .with_client_id("demo-client")
        );

    // Outbound: Temperature readings → MQTT
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to("mqtt://sensors/temperature")
            .with_serializer(|temp: &Temperature| {
                serde_json::to_vec(temp)
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Inbound: MQTT commands → AimDB
    builder.configure::<Command>(|reg| {
        reg.buffer(BufferCfg::Mailbox)
            .link_from("mqtt://commands/device")
            .with_deserializer(|data: &[u8]| {
                serde_json::from_slice::<Command>(data)
                    .map_err(|e| format!("Failed to parse: {}", e))
            })
            .finish();
    });

    // Start database with connector
    builder.run().await?;
    
    // Data flows both ways:
    // MQTT → link_from → AimDB
    // AimDB → link_to → MQTT
    
    Ok(())
}
```

---

## 📚 Documentation

- **Main README**: [README.md](https://github.com/aimdb-dev/aimdb/blob/main/README.md)
- **CHANGELOG**: [CHANGELOG.md](https://github.com/aimdb-dev/aimdb/blob/main/CHANGELOG.md)
- **Migration Guide**: See individual CHANGELOG files for migration paths
- **API Docs**: [docs.rs](https://docs.rs/aimdb) (crates will appear shortly)

---

## 📖 Examples

Updated examples with bidirectional patterns:

1. **tokio-mqtt-connector-demo**: Bidirectional MQTT with Tokio
2. **embassy-mqtt-connector-demo**: Embedded bidirectional MQTT
3. **tokio-knx-connector-demo**: KNX/IP integration (preview)
4. **embassy-knx-connector-demo**: Embedded KNX (preview)
5. **sync-api-demo**: Updated sync API usage
6. **remote-access-demo**: MCP server for introspection

```bash
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
cargo run --example tokio-mqtt-connector-demo
```

---

## 🤝 Contributing

We welcome contributions! See [CONTRIBUTING.md](https://github.com/aimdb-dev/aimdb/blob/main/CONTRIBUTING.md) for guidelines.

**Quick start:**
```bash
git clone https://github.com/aimdb-dev/aimdb.git
cd aimdb
make check  # Run all quality checks
```

---

## 📄 License

Licensed under Apache License 2.0 - see [LICENSE](https://github.com/aimdb-dev/aimdb/blob/main/LICENSE) for details.

---

## 🙏 Acknowledgments

Special thanks to:
- Contributors who provided feedback on the connector API
- The Embassy team for embedded async framework
- KNX community for protocol documentation

---

## 💬 Community

- **Issues**: [GitHub Issues](https://github.com/aimdb-dev/aimdb/issues)
- **Discussions**: [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions)

---

## 🎉 Upgrade Today!

```bash
# Update your dependencies
cargo update aimdb-core aimdb-tokio-adapter aimdb-mqtt-connector

# Or specify v0.2.0 explicitly
cargo add aimdb-core@0.2.0 aimdb-tokio-adapter@0.2.0
```

**Build bidirectional data pipelines across your entire infrastructure!**
