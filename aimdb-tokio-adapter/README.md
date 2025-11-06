# aimdb-tokio-adapter

Tokio async runtime adapter for AimDB - standard library environments.

## Overview

`aimdb-tokio-adapter` provides Tokio-specific extensions for AimDB, enabling the database to run on standard library environments using the Tokio async runtime.

**Key Features:**
- **Tokio Integration**: Seamless integration with Tokio async executor
- **Time Support**: Timestamps, sleep, and delayed tasks with `tokio::time`
- **Task Spawning**: Dynamic task creation with `tokio::task::spawn`
- **Logging**: Structured logging via `tracing` crate
- **Std Compatible**: Full standard library support

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
aimdb-core = "0.1"
aimdb-tokio-adapter = "0.1"
tokio = { version = "1", features = ["full"] }
```

### Basic Example

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct SensorData {
    temperature: f32,
    humidity: f32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create database with Tokio adapter
    let adapter = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<SensorData>(|reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    let db = builder.build()?;
    
    // Use database...
    Ok(())
}
```

## Runtime Traits

The adapter implements all required executor traits:

### RuntimeAdapter
```rust
impl RuntimeAdapter for TokioAdapter {
    fn runtime_name() -> &'static str {
        "tokio"
    }
}
```

### TimeOps
```rust
impl TimeOps for TokioAdapter {
    type Instant = std::time::Instant;
    type Duration = std::time::Duration;

    fn now(&self) -> Self::Instant {
        std::time::Instant::now()
    }
    
    fn millis(&self, millis: u64) -> Self::Duration {
        std::time::Duration::from_millis(millis)
    }
    
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }
    
    // ... other methods
}
```

### Logger
```rust
impl Logger for TokioAdapter {
    fn info(&self, message: &str) {
        println!("ℹ️  {}", message);
    }
    
    fn debug(&self, message: &str) {
        #[cfg(debug_assertions)]
        println!("🔍 {}", message);
    }
    
    fn warn(&self, message: &str) {
        println!("⚠️  {}", message);
    }
    
    fn error(&self, message: &str) {
        eprintln!("❌ {}", message);
    }
}
```

### Spawn
```rust
impl Spawn for TokioAdapter {
    type SpawnToken = tokio::task::JoinHandle<()>;
    
    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Ok(tokio::spawn(future))
    }
}
```

## Features

```toml
[features]
default = ["std", "tokio-runtime"]  # Standard library + Tokio
std = []                             # Standard library support
tokio-runtime = []                   # Tokio runtime integration
tracing = []                         # Structured logging support
metrics = []                         # Performance metrics
test-utils = []                      # Testing utilities
```

## Time Operations

Full time support with std types:

```rust
use aimdb_tokio_adapter::TokioAdapter;
use std::time::Duration;

let adapter = TokioAdapter::new()?;

// Get current instant
let now = adapter.now();

// Create duration
let delay = adapter.millis(1000);

// Sleep for duration
adapter.sleep(delay).await;

// Other duration helpers
let one_sec = adapter.secs(1);
let one_micro = adapter.micros(1000);
```

## Task Spawning

Spawn background tasks using the Spawn trait:

```rust
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_executor::Spawn;
use std::time::Duration;

let adapter = TokioAdapter::new()?;

let handle = adapter.spawn(async {
    // Background task
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("Heartbeat");
    }
})?;

// JoinHandle can be awaited or detached
// Note: Loop above never ends, so this would block forever
// handle.await.unwrap();
```

## Logging

**Note:** The Tokio adapter uses simple `println!`/`eprintln!` for logging by default, **not** `tracing`. 

If you want structured logging with `tracing`, you can enable it via the `tracing` feature in `aimdb-core`:

```toml
[dependencies]
aimdb-core = { version = "0.1", features = ["tracing"] }
aimdb-tokio-adapter = "0.1"
```

Then initialize tracing in your application:

```rust
use tracing_subscriber;

// Initialize tracing
tracing_subscriber::fmt::init();

// Core database operations will use tracing
// Adapter logger still uses println by default
```

Control log levels:
```bash
RUST_LOG=debug cargo run
RUST_LOG=aimdb_core=debug,aimdb_tokio_adapter=trace cargo run
```

## Error Handling

Tokio-specific errors are converted to `ExecutorError`:

```rust
use aimdb_executor::{ExecutorError, ExecutorResult, Spawn};
use aimdb_tokio_adapter::TokioAdapter;

let adapter = TokioAdapter::new()?;

// Task spawn (always succeeds with tokio::spawn)
let handle = adapter.spawn(async {
    println!("Task running");
})?;

// Join handle can fail if task panics
match handle.await {
    Ok(_) => println!("Task completed"),
    Err(e) => eprintln!("Task panicked: {}", e),
}
```

## Performance

Optimized for standard environments:
- **Zero-cost abstractions**: Thin wrapper over Tokio
- **Rich error context**: Full std::error::Error support
- **Heap allocation**: Available for dynamic data structures

## Examples

### Current Thread Runtime

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct Data {
    value: i32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<Data>(|reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    let db = builder.build()?;
    
    // Use database with single-threaded executor
    Ok(())
}
```

## Testing

```bash
# Run tests
cargo test -p aimdb-tokio-adapter

# Run with logging
RUST_LOG=debug cargo test -p aimdb-tokio-adapter -- --nocapture
```

## Complete Examples

See repository examples:
- `examples/tokio-mqtt-connector-demo` - Tokio with MQTT integration
- `examples/remote-access-demo` - Remote introspection server
- `examples/sync-api-demo` - Synchronous wrapper

## Comparison with Embassy Adapter

| Feature              | Tokio Adapter            | Embassy Adapter          |
|----------------------|--------------------------|--------------------------|
| Environment          | std                      | no_std                   |
| Target               | Servers, desktop         | Embedded MCUs            |
| Threading            | Yes                      | No                       |
| Heap allocation      | Unlimited                | Limited/None             |
| Logging              | println (or tracing)     | defmt                    |
| Task spawn           | Dynamic                  | Static pool              |
| SpawnToken type      | `JoinHandle<()>`         | `()`                     |
| Instant type         | std::time::Instant       | embassy Instant          |
| Duration type        | std::time::Duration      | embassy Duration         |

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-tokio-adapter --open
```

## License

See [LICENSE](../LICENSE) file.
