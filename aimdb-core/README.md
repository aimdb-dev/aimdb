# aimdb-core

Core database engine for AimDB - async in-memory storage with real-time synchronization.

## Overview

`aimdb-core` provides the foundational database engine for AimDB, designed for real-time data synchronization across **MCU → edge → cloud** environments with low-latency synchronization.

**Key Features:**
- **Type-Safe Records**: `TypeId`-based routing eliminates string keys and enables compile-time safety
- **Runtime Agnostic**: Works with any async runtime (Tokio, Embassy) via adapter pattern
- **Producer-Consumer Model**: Built-in typed message passing with multiple buffer strategies
- **Pluggable Buffers**: SPMC Ring, SingleLatest, and Mailbox for different data flow patterns
- **No-std Compatible**: Core functionality works without standard library

## Architecture

```
┌─────────────────────────────────────────────────┐
│           Database<A: RuntimeAdapter>           │
│  - Unified API for all operations               │
│  - Type-safe record management                  │
│  - Builder pattern configuration                │
└─────────────────────────────────────────────────┘
                      │
        ┌─────────────┼─────────────┐
        ▼             ▼             ▼
   Producers     Consumers      Connectors
   (Async)       (Async)        (MQTT/Kafka)
```

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
aimdb-core = "0.1"
aimdb-tokio-adapter = "0.1"  # or aimdb-embassy-adapter
```

### Basic Usage

```rust
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_core::buffer::BufferCfg;
use aimdb_tokio_adapter::TokioAdapter;
use std::sync::Arc;

#[tokio::main]
async fn main() -> DbResult<()> {
    // Create runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);
    
    // Build database with typed records
    let mut builder = AimDbBuilder::new().runtime(runtime);
    
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
           .source(temperature_producer)   // Async function that produces data
           .tap(temperature_consumer);      // Async function that consumes data
    });
    
    // Start the database
    let db = builder.build()?;
    db.start().await?;
    
    Ok(())
}
```

For complete working examples with producers, consumers, and connectors, see:
- `examples/tokio-mqtt-connector-demo` - Full producer/consumer with MQTT publishing
- `examples/sync-api-demo` - Synchronous API usage
- `examples/remote-access-demo` - Remote introspection server

## Buffer Types

Choose the right buffer strategy for your use case:

### SPMC Ring Buffer
**Use for:** High-frequency data streams with bounded memory
```rust
use aimdb_core::buffer::BufferCfg;

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
- History doesn't matter

### Mailbox
**Use for:** Commands and one-shot events
```rust
reg.buffer(BufferCfg::Mailbox)
```
- Single-slot with overwrite
- Latest command wins
- At-least-once delivery

## Producer-Consumer Patterns

### Source (Producer)
Async function that generates and sends data:
```rust
async fn my_producer(
    ctx: RuntimeContext<TokioAdapter>,
    producer: Producer<MyData, TokioAdapter>,
) {
    let data = MyData { /* ... */ };
    producer.produce(data).await.ok();
}

// Register in builder
reg.buffer(BufferCfg::SingleLatest)
   .source(my_producer);
```

### Tap (Consumer)
Async function that receives and processes data:
```rust
async fn my_consumer(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: Consumer<MyData, TokioAdapter>,
) {
    let mut reader = consumer.subscribe().expect("Failed to subscribe");
    
    while let Ok(data) = reader.recv().await {
        // Process data
    }
}

// Register in builder
reg.tap(my_consumer);
```

For complete examples, see `examples/tokio-mqtt-connector-demo`.

## Type Safety

Records are identified by `TypeId`, not strings:

```rust
use std::any::TypeId;

let type_id = TypeId::of::<Temperature>();
// TypeId automatically used for record lookup
```

**Benefits:**
- Compile-time type checking
- No string parsing overhead
- Impossible to mix types

## Runtime Adapters

Core depends on abstract traits from `aimdb-executor`:

- **TokioAdapter**: Standard library environments
- **EmbassyAdapter**: Embedded no_std environments

Adapters provide:
- Task spawning (`Spawn`)
- Time operations (`TimeOps`)
- Logging (`Logger`)
- Platform identification (`RuntimeAdapter`)

## Features

```toml
[features]
std = []                    # Standard library support
tracing = ["dep:tracing"]   # Structured logging
metrics = []                # Performance metrics
defmt = ["dep:defmt"]       # Embedded logging
```

## Error Handling

All operations return `DbResult<T>` with `DbError` enum:

```rust
use aimdb_core::{DbResult, DbError};

pub async fn operation() -> DbResult<()> {
    // ... operations ...
    Ok(())
}
```

Common errors:
- `RecordNotFound`: Requested type not registered
- `ProducerFailed` / `ConsumerFailed`: Task execution errors
- `BufferError`: Buffer operations failed
- `ConnectorError`: External connector issues

## Connectors

Integrate with external systems like MQTT, Kafka, or custom protocols:

```rust
use aimdb_mqtt_connector::MqttConnector;
use std::sync::Arc;

let mqtt = Arc::new(MqttConnector::new("mqtt://localhost:1883").await?);

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector("mqtt", mqtt);

builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .source(temperature_producer)
       .link("mqtt://sensors/temperature")
       .with_serializer(|t| serde_json::to_vec(t).map_err(|_| SerializeError::InvalidData))
       .finish();
});

let db = builder.build()?;
```

See `examples/tokio-mqtt-connector-demo` for complete connector integration.

## Testing

```bash
# Run tests (std)
cargo test -p aimdb-core

# Run tests (no_std embedded)
cargo test -p aimdb-core --no-default-features --target thumbv7em-none-eabihf
```

## Performance

Design targets:
- **< 50ms** end-to-end latency
- **Lock-free** buffer operations
- **Zero-copy** where possible
- **Minimal allocations** in hot paths

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-core --open
```

## License

See [LICENSE](../LICENSE) file.
