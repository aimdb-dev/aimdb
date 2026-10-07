# aimdb-sync

Synchronous API wrapper for AimDB.

## Overview

`aimdb-sync` provides a synchronous interface to AimDB for code that does not use an async executor. Perfect for FFI bindings, legacy codebases, simple scripts, and situations where async is impractical.

**Key Features:**
- **Pure Sync Context**: Works in plain `fn main()` - no `#[tokio::main]` required. Blocking calls panic if made from async code inside a Tokio runtime
- **Synchronous API**: Produce and consume records without `async` or `await`
- **Thread-Safe**: `SyncProducer` type is `Send + Sync`, shareable across threads. `SyncConsumer` type is `Send` only, can be moved to another thread.
- **Type-Safe**: Full compile-time type safety with generics
- **Timeout Support**: Blocking consumer read operations support configurable timeouts

## Architecture

Producers write directly to AimDB record buffers. Consumers read from the
buffers through `Reader<T>`. A background thread runs AimDB's async tasks.

```
┌──────────────────────────────┐
│   Synchronous Context        │
│   (User Code)                │
└──────────────┬───────────────┘
               │ SyncProducer<T>
               │ SyncConsumer<T>
               ▼
┌──────────────────────────────┐
│   AimDB Record Buffer        │
└──────────────┬───────────────┘
               | shared with
               ▼
┌──────────────────────────────┐
│   Async Context              │
│   (AimDB + Tokio Runtime)    │
│   (Background Thread)        │
└──────────────────────────────┘
```

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
aimdb-sync = "0.6"
aimdb-core = "2.0"
aimdb-tokio-adapter = "0.7"
```

### Basic Example

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
struct Temperature {
    celsius: f32,
    sensor_id: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build database and attach for sync API (starts background runtime)
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<Temperature>("temperature", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    let handle = builder.attach()?;
    
    // Get sync handles
    let producer = handle.producer::<Temperature>("temperature")?;
    let mut consumer = handle.consumer::<Temperature>("temperature")?;
    
    // Send from one thread
    let prod_handle = std::thread::spawn(move || {
        for i in 0..10 {
            let temp = Temperature {
                celsius: 20.0 + i as f32,
                sensor_id: format!("sensor-{}", i),
            };
            producer.set(temp).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    
    // Receive from another thread
    let cons_handle = std::thread::spawn(move || {
        for _ in 0..10 {
            let temp = consumer.get().unwrap();
            println!("Temperature: {}°C from {}", temp.celsius, temp.sensor_id);
        }
    });
    
    prod_handle.join().unwrap();
    cons_handle.join().unwrap();
    
    // Clean shutdown
    handle.detach()?;
    
    Ok(())
}
```

## Producer Operations

`SyncProducer<T>` writes directly to the configured record buffer:

### Write a Value

```rust
let producer = handle.producer::<Temperature>("temperature")?;

let temp = Temperature { 
    celsius: 23.5, 
    sensor_id: "sensor-001".to_string() 
};

// Writes directly to the configured record buffer
producer.set(temp)?;
```

## Consumer Operations

`SyncConsumer<T>` provides blocking receive operations:

### Blocking Receive

```rust
let mut consumer = handle.consumer::<Temperature>("temperature")?;

// Blocks until value is available
let temp = consumer.get()?;
println!("Received: {}°C", temp.celsius);
```

### Receive with Timeout

```rust
use std::time::Duration;

// Wait max 5 seconds
match consumer.get_with_timeout(Duration::from_secs(5)) {
    Ok(temp) => println!("Got: {}°C", temp.celsius),
    Err(e) => eprintln!("Timeout or error: {}", e),
}
```

### Non-Blocking Receive

```rust
// Returns immediately
match consumer.try_get() {
    Ok(temp) => println!("Got: {}°C", temp.celsius),
    Err(e) => eprintln!("No value available: {}", e),
}
```

### Latest Value

`get_latest()` waits for one value, then returns the newest available value.
`get_latest_with_timeout()` applies its timeout only while waiting for the
first value. Both methods skip `BufferLagged` while catching up.

```rust
let latest = consumer.get_latest()?;

let latest_with_timeout =
    consumer.get_latest_with_timeout(Duration::from_secs(1))?;
```

## Multi-Consumer Pattern

Multiple consumers can receive from the same record:

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<Temperature>("temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    
    let handle = builder.attach()?;
    let producer = handle.producer::<Temperature>("temperature")?;
    
    // Spawn multiple consumer threads
    let mut handles = vec![];
    
    for id in 0..3 {
        let mut consumer = handle.consumer::<Temperature>("temperature")?;
        let handle = std::thread::spawn(move || {
            loop {
                match consumer.get_with_timeout(Duration::from_secs(1)) {
                    Ok(temp) => println!("Consumer {}: {}°C", id, temp.celsius),
                    Err(_) => break,
                }
            }
        });
        handles.push(handle);
    }
    
    // Producer sends
    for i in 0..10 {
        producer.set(Temperature { 
            celsius: 20.0 + i as f32,
            sensor_id: "main".to_string(),
        })?;
        std::thread::sleep(Duration::from_millis(100));
    }
    
    for handle in handles {
        handle.join().unwrap();
    }
    
    handle.detach()?;
    
    Ok(())
}
```

## Thread Safety

`SyncProducer` type is `Send + Sync`, shareable across threads. `SyncConsumer` type is `Send` only, can be moved to another thread:

```rust
let producer = handle.producer::<Temperature>("temperature")?;
let mut consumer = handle.consumer::<Temperature>("temperature")?;

// `SyncProducer` implements `Clone`
let prod_clone = producer.clone();
std::thread::spawn(move || {
    prod_clone.set(Temperature { celsius: 25.0, sensor_id: "s1".to_string() }).ok();
});

// `SyncConsumer` implements `Send`, can be moved to another thread
std::thread::spawn(move || {
    consumer.get().ok();
});
```

## Error Handling

```rust
use aimdb_sync::SyncError;

match producer.set(temp) {
    Ok(_) => println!("Success"),
    Err(SyncError::RuntimeShutdown) => {
        eprintln!("Runtime thread has stopped");
    }
    Err(e) => {
        eprintln!("Error: {}", e);
    }
}
```

Common error types:
- `SyncError::GetTimeout`: Operation exceeded timeout
- `SyncError::RuntimeShutdown`: Runtime thread stopped
- `SyncError::ForkedChild`: A forked child no longer has the runtime thread
- `SyncError::Db(DbError::RecordKeyNotFound {...})`: Type not registered in database
- `SyncError::Db(DbError::BufferLagged {...})`: An SPMC consumer missed overwritten values
- `SyncError::AttachFailed`: Failed to start runtime thread

`set()` reports producer key and type errors. `handle.consumer()` reports
equivalent consumer errors during construction.

## Configuration Options

### Buffer Types

Choose buffer based on use case:

```rust
use aimdb_core::buffer::BufferCfg;

// SPMC Ring: Multiple consumers, bounded history, overwrites the oldest value when full
builder.configure::<MyData>("my-data", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
});

// SingleLatest: Always get newest value, each write replaces the previous value
builder.configure::<MyData>("my-data", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
});

// Mailbox: One pending value. A new value overwrites an unread one, and with
// several consumers each value is taken by only one of them
builder.configure::<MyData>("my-data", |reg| {
    reg.buffer(BufferCfg::Mailbox);
});
```

## Shutdown

Call `detach()` to stop and join the runtime thread. Dropping the handle only
signals shutdown and does not wait:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = builder.attach()?;
    
    // Use database...
    
    // Explicit shutdown (recommended)
    handle.detach()?;
    
    // Or with timeout
    // handle.detach_timeout(Duration::from_secs(5))?;
    
    // Dropping only signals shutdown
    // drop(handle);
    
    Ok(())
}
```

## Use Cases

### Legacy Integration

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::{AimDbBuilderSyncExt, AimDbHandle, SyncConsumer, SyncProducer};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

// Wrap async AimDB for legacy sync code
pub struct LegacyAdapter {
    handle: AimDbHandle,
    producer: SyncProducer<SensorData>,
    // Kept for the adapter's lifetime: a new SPMC consumer only sees values
    // produced after it subscribes.
    consumer: SyncConsumer<SensorData>,
}

impl LegacyAdapter {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let adapter = Arc::new(TokioAdapter);
        let mut builder = AimDbBuilder::new().runtime(adapter);
        
        builder.configure::<SensorData>("sensor-data", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
        });
        
        let handle = builder.attach()?;
        let producer = handle.producer::<SensorData>("sensor-data")?;
        let consumer = handle.consumer::<SensorData>("sensor-data")?;
        Ok(Self { handle, producer, consumer })
    }
    
    pub fn send_sensor_data(&self, data: SensorData) -> Result<(), String> {
        self.producer.set(data)
            .map_err(|e| e.to_string())
    }
    
    pub fn read_sensor_data(&mut self) -> Result<SensorData, String> {
        self.consumer.get_with_timeout(Duration::from_secs(1))
            .map_err(|e| e.to_string())
    }
    
    pub fn shutdown(self) -> Result<(), String> {
        self.handle.detach().map_err(|e| e.to_string())
    }
}
```

### Simple Scripts

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

// Quick script without async complexity
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<LogMessage>("log-message", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
    });
    
    let handle = builder.attach()?;
    let producer = handle.producer::<LogMessage>("log-message")?;
    let mut consumer = handle.consumer::<LogMessage>("log-message")?;
    
    // Simple loop - no async/await
    loop {
        let log = read_log_from_file();
        producer.set(log)?;
        
        if let Ok(msg) = consumer.try_get() {
            print_to_console(msg);
        }
        
        std::thread::sleep(Duration::from_millis(100));
    }
}
```

## Performance Considerations

### Overhead
- `attach()` starts a dedicated background thread that owns and drives the Tokio runtime.

### Optimization Tips
- **Avoid Blocking**: Use `SyncConsumer::try_get()` when the caller thread must not wait.

## Testing

```bash
# Run tests
cargo test -p aimdb-sync

# Run with logging
RUST_LOG=debug cargo test -p aimdb-sync -- --nocapture

```

## Complete Examples

See repository examples:
- `examples/sync-api-demo` - Full synchronous integration

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-sync --open
```

## License

See [LICENSE](../LICENSE) file.
