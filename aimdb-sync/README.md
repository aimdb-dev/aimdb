# aimdb-sync

Synchronous API wrapper for AimDB - blocking operations for async database.

## Overview

`aimdb-sync` provides a synchronous interface to AimDB, enabling blocking operations on the async database. Perfect for FFI bindings, legacy codebases, simple scripts, and situations where async is impractical.

**Key Features:**
- **Pure Sync Context**: Works in plain `fn main()` - no `#[tokio::main]` required
- **Blocking Operations**: Familiar sync API (set, get, try_get, etc.)
- **Thread-Safe**: All types are `Send + Sync`, shareable across threads
- **Type-Safe**: Full compile-time type safety with generics
- **Timeout Support**: All operations support configurable timeouts

## Architecture

```
┌──────────────────────────────┐
│   Synchronous Context        │
│   (User Code)                │
└──────────────┬───────────────┘
               │ SyncProducer<T>
               │ SyncConsumer<T>
               ▼
┌──────────────────────────────┐
│   Channel Bridge             │
│   (tokio::sync::mpsc +       │
│    std::sync::mpsc)          │
└──────────────┬───────────────┘
               │
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
aimdb-sync = "0.1"
aimdb-core = "0.1"
aimdb-tokio-adapter = "0.1"
```

### Basic Example

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temperature {
    celsius: f32,
    sensor_id: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build database and attach for sync API (starts background runtime)
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    let handle = builder.attach()?;
    
    // Get sync handles
    let producer = handle.producer::<Temperature>()?;
    let consumer = handle.consumer::<Temperature>()?;
    
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

`SyncProducer<T>` provides blocking send operations:

### Blocking Send

```rust
let producer = handle.producer::<Temperature>()?;

let temp = Temperature { 
    celsius: 23.5, 
    sensor_id: "sensor-001".to_string() 
};

// Blocks until send completes
producer.set(temp)?;
```

### Send with Timeout

```rust
use std::time::Duration;

// Block for max 1 second
match producer.set_with_timeout(temp, Duration::from_secs(1)) {
    Ok(_) => println!("Sent successfully"),
    Err(e) => eprintln!("Timeout or error: {}", e),
}
```

### Non-Blocking Send

```rust
// Returns immediately (doesn't wait for produce to complete)
match producer.try_set(temp) {
    Ok(_) => println!("Sent immediately"),
    Err(e) => eprintln!("Channel full or error: {}", e),
}
```

## Consumer Operations

`SyncConsumer<T>` provides blocking receive operations:

### Blocking Receive

```rust
let consumer = handle.consumer::<Temperature>()?;

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
    
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    
    let handle = builder.attach()?;
    let producer = handle.producer::<Temperature>()?;
    
    // Spawn multiple consumer threads
    let mut handles = vec![];
    
    for id in 0..3 {
        let consumer = handle.consumer::<Temperature>()?;
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

All sync types are `Send + Sync`:

```rust
use std::sync::Arc;

let producer = Arc::new(handle.producer::<Temperature>()?);
let consumer = Arc::new(handle.consumer::<Temperature>()?);

// Share across threads
let prod_clone = producer.clone();
std::thread::spawn(move || {
    prod_clone.set(Temperature { celsius: 25.0, sensor_id: "s1".to_string() }).ok();
});

let cons_clone = consumer.clone();
std::thread::spawn(move || {
    let value = cons_clone.get().ok();
});
```

## Error Handling

```rust
use aimdb_core::DbError;

match producer.set(temp) {
    Ok(_) => println!("Success"),
    Err(DbError::SetTimeout) => {
        eprintln!("Operation timed out");
    }
    Err(DbError::RuntimeShutdown) => {
        eprintln!("Runtime thread has stopped");
    }
    Err(e) => {
        eprintln!("Error: {}", e);
    }
}
```

Common error types:
- `DbError::SetTimeout` / `DbError::GetTimeout`: Operation exceeded timeout
- `DbError::RuntimeShutdown`: Runtime thread stopped or channel closed
- `DbError::RecordNotFound`: Type not registered in database
- `DbError::AttachFailed`: Failed to start runtime thread

## Configuration Options

### Buffer Types

Choose buffer based on use case:

```rust
use aimdb_core::buffer::BufferCfg;

// SPMC Ring: Multiple consumers, bounded history
builder.configure::<MyData>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
});

// SingleLatest: Always get newest value
builder.configure::<MyData>(|reg| {
    reg.buffer(BufferCfg::SingleLatest);
});

// Mailbox: Single slot, overwrite
builder.configure::<MyData>(|reg| {
    reg.buffer(BufferCfg::Mailbox);
});
```

### Channel Capacity

Control the sync bridge channel size:

```rust
// Default capacity (100)
let producer = handle.producer::<Temperature>()?;
let consumer = handle.consumer::<Temperature>()?;

// Custom capacity for high-frequency data
let producer = handle.producer_with_capacity::<Temperature>(1000)?;
let consumer = handle.consumer_with_capacity::<Temperature>(1000)?;
```

## Shutdown

Database automatically shuts down when dropped:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = builder.attach()?;
    
    // Use database...
    
    // Explicit shutdown (recommended)
    handle.detach()?;
    
    // Or with timeout
    // handle.detach_timeout(Duration::from_secs(5))?;
    
    // Or just drop (automatic cleanup with warning)
    // drop(handle);
    
    Ok(())
}
```

## Use Cases

### Legacy Integration

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_sync::{AimDbBuilderSyncExt, AimDbHandle};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

// Wrap async AimDB for legacy sync code
pub struct LegacyAdapter {
    handle: AimDbHandle,
}

impl LegacyAdapter {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let adapter = Arc::new(TokioAdapter);
        let mut builder = AimDbBuilder::new().runtime(adapter);
        
        builder.configure::<SensorData>(|reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
        });
        
        let handle = builder.attach()?;
        Ok(Self { handle })
    }
    
    pub fn send_sensor_data(&self, data: SensorData) -> Result<(), String> {
        let producer = self.handle.producer::<SensorData>()
            .map_err(|e| e.to_string())?;
        
        producer.set_with_timeout(data, Duration::from_secs(1))
            .map_err(|e| e.to_string())
    }
    
    pub fn read_sensor_data(&self) -> Result<SensorData, String> {
        let consumer = self.handle.consumer::<SensorData>()
            .map_err(|e| e.to_string())?;
        
        consumer.get_with_timeout(Duration::from_secs(1))
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
    
    builder.configure::<LogMessage>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
    });
    
    let handle = builder.attach()?;
    let producer = handle.producer::<LogMessage>()?;
    let consumer = handle.consumer::<LogMessage>()?;
    
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
- Channel crossing adds ~1-10μs latency
- Background runtime uses dedicated threads
- Memory: One tokio::mpsc channel per producer, one std::mpsc channel per consumer

### Optimization Tips
1. **Batch Operations**: Group multiple sets/gets when possible
2. **Avoid Blocking**: Use `try_*` methods in latency-sensitive paths
3. **Channel Capacity**: Tune for expected throughput
4. **Thread Count**: Match runtime_threads to workload

## Testing

```bash
# Run tests
cargo test -p aimdb-sync

# Run with logging
RUST_LOG=debug cargo test -p aimdb-sync -- --nocapture

# Benchmark
cargo bench -p aimdb-sync
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
