# aimdb-core

Type-safe async data pipelines — one Rust codebase from MCU to cloud.

## Overview

`aimdb-core` provides a type-safe, platform-agnostic data pipeline where the Rust type system is the schema and trait implementations define the behavior. Records flow through typed producer-consumer pipelines that compile unchanged for Tokio, Embassy and WASM.

**Key Features:**
- **Type-Safe Records**: Compile-time key + type pairs
- **Runtime Agnostic**: One codebase targets Tokio, Embassy and WASM via adapter pattern
- **Producer-Consumer Pipelines**: Source, tap and transform with typed buffers
- **Pluggable Buffers**: SPMC Ring, SingleLatest and Mailbox for different data flow patterns
- **No-std Compatible**: Core functionality works without standard library
- **Derive Macros**: `RecordKey` derive for compile-time safe record keys

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
aimdb-core = "1.0"
aimdb-tokio-adapter = "0.5"  # or aimdb-embassy-adapter / aimdb-wasm-adapter
```

### Define Records

Records consist of a key and a type. The `RecordKey` derive macro maps each variant to a unique string identifier:

```rust
/// Compile-time safe record keys.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[key_prefix = "sensor."]
pub enum SensorKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
}

/// A plain Rust struct — no framework traits required.
#[derive(Clone, Debug)]
pub struct Temperature {
    pub sensor_id: String,
    pub celsius: f32,
}
```

### Producer and Consumer

Portable code receives the concrete `RuntimeContext` — no platform imports needed:

```rust
/// Producer: reads a sensor and pushes typed values into AimDB.
async fn sensor_producer(ctx: RuntimeContext, producer: Producer<Temperature>) {
    loop {
        let reading = read_sensor().await;
        producer.produce(Temperature {
            sensor_id: "outdoor-001".into(),
            celsius: reading,
        }).await.ok();
        ctx.time().sleep_secs(1).await;
    }
}

/// Consumer: subscribes to the buffer and reacts to every new value.
async fn temp_logger(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let mut reader = consumer.subscribe().unwrap();
    while let Ok(temp) = reader.recv().await {
        ctx.log().info(&format!("{}: {:.1}°C", temp.sensor_id, temp.celsius));
    }
}
```

### Wire It Together

The builder ties key, type, buffer, producer and consumer together:

```rust
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_core::buffer::BufferCfg;
use aimdb_tokio_adapter::TokioAdapter;
use std::sync::Arc;

#[tokio::main]
async fn main() -> DbResult<()> {
    let runtime = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>(SensorKey::TempOutdoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
           .source(sensor_producer)
           .tap(temp_logger)
           .finish();
    });

    let db = builder.build()?;
    db.start().await?;
    Ok(())
}
```

### Transforms

Records relate to each other in a DAG. A transform derives one record type from another:

```rust
#[derive(Clone, Debug)]
pub struct TemperatureAlert {
    pub sensor_id: String,
    pub celsius: f32,
    pub level: &'static str,
}

fn to_alert(temp: &Temperature) -> Option<TemperatureAlert> {
    if temp.celsius > 35.0 {
        Some(TemperatureAlert {
            sensor_id: temp.sensor_id.clone(),
            celsius: temp.celsius,
            level: "high",
        })
    } else {
        None
    }
}

builder.configure::<TemperatureAlert>("alert.temp.outdoor", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .transform::<Temperature, _>(SensorKey::TempOutdoor, |t| t.map(to_alert))
       .tap(alert_logger)
       .finish();
});
```

For complete working examples, see:
- `examples/tokio-mqtt-connector-demo` — MQTT with Tokio runtime
- `examples/embassy-mqtt-connector-demo` — MQTT on embedded (RP2040)
- `examples/sync-api-demo` — Synchronous API usage
- `examples/remote-access-demo` — AimX protocol server

## Buffer Types

Choose the right buffer strategy for your use case:

### SPMC Ring Buffer
**Use for:** High-frequency data streams with bounded memory
```rust
reg.buffer(BufferCfg::SpmcRing { capacity: 2048 })
```
- Bounded backlog for multiple consumers
- Fast producers can outrun slow consumers
- Oldest messages dropped on overflow

### SingleLatest
**Use for:** State synchronization, configuration updates
```rust
reg.buffer(BufferCfg::SingleLatest)
```
- Only stores newest value
- Intermediate updates collapsed

### Mailbox
**Use for:** Commands and one-shot events
```rust
reg.buffer(BufferCfg::Mailbox)
```
- Single-slot with overwrite
- Latest command wins

## Runtime Adapters

The only platform-specific code is choosing which runtime adapter to pass into the builder:

```rust
// On Linux / Cloud — Tokio
let runtime = Arc::new(TokioAdapter::new()?);
let mut builder = AimDbBuilder::new().runtime(runtime);

// On Cortex-M4 — Embassy
let runtime = Arc::new(EmbassyAdapter::new());
let mut builder = AimDbBuilder::new().runtime(runtime);
```

Everything else — records, keys, producers, consumers, transforms — stays identical across platforms.

Core consumes the runtime through `aimdb-executor`'s dyn-safe `RuntimeOps`
trait (name, monotonic clock, wall clock, sleep, log) — adapters implement it
once and the runtime travels as a value (`Arc<dyn RuntimeOps>`).

Portable code receives the concrete `RuntimeContext` with two accessors:
- `ctx.time()` → `Time` with `.sleep(Duration)` / `.sleep_millis()` / `.sleep_secs()`, `.now()` (u64 nanos), `.unix_time()`
- `ctx.log()` → `Log` with `.info()`, `.warn()`, etc.

Available adapters:
- **[aimdb-tokio-adapter](https://crates.io/crates/aimdb-tokio-adapter)** — Standard library / server environments
- **[aimdb-embassy-adapter](https://crates.io/crates/aimdb-embassy-adapter)** — Embedded `no_std` (Cortex-M)
- **[aimdb-wasm-adapter](https://crates.io/crates/aimdb-wasm-adapter)** — Browser / WASM targets

## Connectors

Integrate with external systems:
- **[aimdb-mqtt-connector](https://crates.io/crates/aimdb-mqtt-connector)** — MQTT for std (`rumqttc`) and embedded (`mountain-mqtt`)
- **[aimdb-knx-connector](https://crates.io/crates/aimdb-knx-connector)** — KNX/IP for building automation
- **[aimdb-websocket-connector](https://crates.io/crates/aimdb-websocket-connector)** — WebSocket transport

See `examples/tokio-mqtt-connector-demo` for a complete connector integration.

## Features

```toml
[features]
default = ["std", "alloc", "derive"]
derive = ["aimdb-derive"]           # RecordKey derive macro
std = ["alloc", "serde", ...]       # Standard library support
alloc = ["serde"]                   # Heap allocation for no_std
tracing = ["dep:tracing"]           # Structured logging (std + no_std)
defmt = ["dep:defmt"]               # Embedded logging via probe
metrics = ["dep:metrics", "std"]    # Performance metrics (requires std)
test-utils = ["std"]                # Test helpers
```

## Error Handling

All operations return `DbResult<T>` with `DbError` enum:

```rust
use aimdb_core::{DbResult, DbError};

pub async fn operation() -> DbResult<()> {
    // ...
    Ok(())
}
```

Common errors: `RecordNotFound`, `ProducerFailed`, `ConsumerFailed`, `BufferError`, `ConnectorError`.

## Testing

```bash
# Run all tests
cargo test -p aimdb-core

# Verify no_std compatibility
cargo check -p aimdb-core --no-default-features --target thumbv7em-none-eabihf
```

## License

Apache 2.0 — see [LICENSE](../LICENSE).
