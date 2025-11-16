# aimdb-mqtt-connector

MQTT connector for AimDB - supports both std (Tokio) and no_std (Embassy) environments.

## Overview

`aimdb-mqtt-connector` provides MQTT publishing capabilities for AimDB records with automatic consumer registration. Works seamlessly across standard library (Tokio) and embedded (Embassy) environments.

**Key Features:**
- **Dual Runtime Support**: Works with both Tokio and Embassy
- **Automatic Consumer Registration**: Connects to records via builder pattern
- **Topic Mapping**: Flexible record-to-topic configuration
- **Custom Serialization**: Pluggable serializers (JSON, MessagePack, etc.)
- **QoS Control**: Configure MQTT quality of service levels

## Quick Start

### Tokio (Standard Library)

Add to your `Cargo.toml`:
```toml
[dependencies]
aimdb-core = "0.1"
aimdb-tokio-adapter = "0.1"
aimdb-mqtt-connector = { version = "0.1", features = ["tokio-runtime"] }
```

Example:
```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_mqtt_connector::MqttConnector;
use std::sync::Arc;

#[derive(Clone, serde::Serialize)]
struct Temperature {
    celsius: f32,
    sensor_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);
    
    // Create MQTT connector
    let mqtt = Arc::new(MqttConnector::new("mqtt://localhost:1883").await?);
    
    // Build database with connector
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector("mqtt", mqtt);
    
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
           .link("mqtt://sensors/temperature")
           .with_serializer(|t| {
               serde_json::to_vec(t)
                   .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
           })
           .finish();
    });
    
    builder.run().await
}
```

### Embassy (Embedded)

Add to your `Cargo.toml`:
```toml
[dependencies]
aimdb-core = { version = "0.1", default-features = false }
aimdb-embassy-adapter = { version = "0.1", default-features = false }
aimdb-mqtt-connector = { version = "0.1", default-features = false, features = ["embassy-runtime"] }
```

Example:
```rust
#![no_std]
#![no_main]

use aimdb_core::AimDbBuilder;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExt};
use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
use alloc::sync::Arc;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize network stack
    let stack = /* ... */;
    
    // Create runtime adapter with network stack access
    let runtime = Arc::new(EmbassyAdapter::new_with_network(spawner, stack));
    
    // Build database with MQTT connector - background tasks spawn automatically
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(MqttConnectorBuilder::new("mqtt://192.168.1.100:1883"));
    
    builder.configure::<SensorData>(|reg| {
        reg.buffer_sized::<4, 1>(EmbassyBufferType::SingleLatest)
           .source(sensor_producer)
           // Outbound: Publish to MQTT
           .link_to("mqtt://data/sensor")
           .with_serializer(|data| {
               // Use postcard or similar no_std serializer
               postcard::to_vec(data)
                   .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
           })
           .finish()
           // Inbound: Subscribe from MQTT
           .link_from("mqtt://commands/sensor")
           .with_deserializer(|data| SensorCommand::from_bytes(data))
           .finish();
    });
    
    let _db = builder.build().await.unwrap();
    
    // Database and MQTT run in background automatically
    loop {
        // Main application loop
    }
}
```

## Architecture

```
┌─────────────────────┐
│   AimDB Record      │
│   (Temperature)     │
└──────────┬──────────┘
           │
           ▼
    ┌──────────────┐
    │  Consumer    │
    │ (auto-reg)   │
    └──────┬───────┘
           │
           ▼
┌──────────────────────┐
│  MQTT Connector      │
│  - Serialize         │
│  - Publish           │
└──────────┬───────────┘
           │
           ▼
    MQTT Broker
```

## Configuration

### Connector Creation

```rust
// Tokio - simple broker URL
let mqtt = MqttConnector::new("mqtt://localhost:1883").await?;

// Tokio - secure connection
let mqtt = MqttConnector::new("mqtts://broker.example.com:8883").await?;

// Note: Client ID is auto-generated, credentials extracted from URL if provided
// Example with credentials: mqtt://username:password@broker:1883
```

### Link Configuration

```rust
// Basic link with JSON serialization
reg.link("mqtt://topic/path")
   .with_serializer(|data| {
       serde_json::to_vec(data)
           .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
   })
   .finish();

// Override QoS and retain per link
reg.link("mqtt://topic/path")
   .with_qos(1)          // QoS 1
   .with_retain(true)    // Retain message
   .with_serializer(|data| {
       serde_json::to_vec(data)
           .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
   })
   .finish();
```

## Topic Mapping

### Static Topics
```rust
// Simple topic
reg.link("mqtt://sensors/temperature")

// Nested topics
reg.link("mqtt://building/floor1/room5/temperature")
```

### Dynamic Topics (Planned)
```rust
// Template-based (future feature)
reg.link("mqtt://sensors/{sensor_id}/temperature")
```

## Serialization

### JSON (std)
```rust
.with_serializer(|data| {
    serde_json::to_vec(data)
        .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
})
```

### MessagePack (std)
```rust
.with_serializer(|data| {
    rmp_serde::to_vec(data)
        .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
})
```

### Postcard (no_std)
```rust
.with_serializer(|data| {
    postcard::to_vec(data)
        .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
})
```

### Custom Binary
```rust
.with_serializer(|data: &Temperature| {
    let mut buf = Vec::new();
    buf.extend_from_slice(&data.celsius.to_le_bytes());
    buf.extend_from_slice(data.sensor_id.as_bytes());
    Ok(buf)
})
```

## MQTT Libraries Used

### Standard Library (Tokio)
- **rumqttc**: Async MQTT client with robust reconnection
- Supports MQTT 3.1.1 and 5.0
- TLS support available

### Embedded (Embassy)
- **mountain-mqtt**: no_std MQTT client
- Optimized for resource-constrained devices
- Supports MQTT 3.1.1

## QoS Levels

MQTT Quality of Service levels are configured using integers:

```rust
.with_qos(0)  // AtMostOnce - Fire and forget
.with_qos(1)  // AtLeastOnce - Acknowledged delivery (default)
.with_qos(2)  // ExactlyOnce - Assured delivery
```

**Recommendations:**
- **QoS 0**: High-frequency sensor data, low-priority logs
- **QoS 1**: Commands, important events (default)
- **QoS 2**: Critical state changes, financial transactions

## Error Handling

```rust
pub enum MqttError {
    InvalidUrl(String),
    ConnectionFailed(String),
    PublishFailed(String),
    SubscriptionFailed(String),
    MissingConfig(String),
    DbError(DbError),
}
```

Serialization errors are returned via `SerializeError`:
```rust
pub enum SerializeError {
    InvalidData,
    TypeMismatch,
}
```

The connector automatically handles reconnection. Serialization errors will be logged and the producer will continue operating.
```

## Features

```toml
[features]
tokio-runtime = ["dep:rumqttc", "dep:tokio"]      # Tokio support
embassy-runtime = ["dep:mountain-mqtt"]           # Embassy support
tracing = ["dep:tracing"]                         # Logging (std)
defmt = ["dep:defmt"]                             # Logging (embedded)
```

## Connection Management

### Automatic Reconnection
The connector automatically handles:
- Initial connection failures
- Network interruptions
- Broker restarts

### Backpressure
When broker is unavailable:
- Messages queue internally
- Producer may block if buffer full
- Configurable buffer size

## Testing

### Tokio Tests
```bash
# Start MQTT broker
docker run -d -p 1883:1883 eclipse-mosquitto

# Run tests
cargo test -p aimdb-mqtt-connector --features tokio-runtime
```

### Embassy Tests
```bash
# Cross-compile test
cargo build -p aimdb-mqtt-connector \
    --target thumbv7em-none-eabihf \
    --no-default-features \
    --features embassy-runtime
```

## Examples

### Multi-Record Publishing

```rust
let runtime = Arc::new(TokioAdapter::new()?);
let mqtt = Arc::new(MqttConnector::new("mqtt://localhost:1883").await?);

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector("mqtt", mqtt);

builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://sensors/temperature")
       .with_serializer(json_serializer)
       .finish();
});

builder.configure::<Humidity>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://sensors/humidity")
       .with_serializer(json_serializer)
       .finish();
});

builder.configure::<Pressure>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://sensors/pressure")
       .with_serializer(json_serializer)
       .finish();
});
```

### Custom QoS per Record

```rust
builder.configure::<HighPriorityAlert>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://alerts/critical")
       .with_qos(2)         // QoS 2 - ExactlyOnce
       .with_retain(true)
       .with_serializer(json_serializer)
       .finish();
});

builder.configure::<SensorTelemetry>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
       .link("mqtt://telemetry/raw")
       .with_qos(0)         // QoS 0 - AtMostOnce
       .with_retain(false)
       .with_serializer(json_serializer)
       .finish();
});
```

## Complete Examples

See repository examples:
- `examples/tokio-mqtt-connector-demo` - Full Tokio MQTT integration
- `examples/embassy-mqtt-connector-demo` - RP2040 with WiFi MQTT

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-mqtt-connector --open
```

## License

See [LICENSE](../LICENSE) file.
