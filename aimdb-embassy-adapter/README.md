# aimdb-embassy-adapter

Embassy async runtime adapter for AimDB - embedded no_std environments.

## Overview

`aimdb-embassy-adapter` provides Embassy-specific extensions for AimDB, enabling the database to run on embedded systems using the Embassy async runtime and embedded-hal peripheral abstractions.

**Key Features:**
- **Embassy Integration**: Seamless integration with Embassy async executor
- **No-std Compatible**: Designed for resource-constrained embedded systems
- **Configurable Task Pool**: Adjustable pool size for dynamic task spawning
- **Hardware Abstraction**: Support for embedded-hal peripherals
- **Defmt Logging**: Efficient logging for embedded debugging

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
aimdb-core = { version = "0.1", default-features = false }
aimdb-embassy-adapter = { version = "0.1", default-features = false, features = ["embassy-task-pool-16"] }
embassy-executor = { version = "0.6", features = ["arch-cortex-m", "executor-thread"] }
embassy-time = "0.3"
```

### Basic Example

```rust
#![no_std]
#![no_main]

use aimdb_core::AimDbBuilder;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyRecordRegistrarExt};
use embassy_executor::Spawner;
use defmt_rtt as _;
use panic_probe as _;

#[derive(Clone, Debug)]
struct SensorReading {
    value: f32,
    timestamp: u32,
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Create adapter with spawner for task management
    let adapter = EmbassyAdapter::new_with_spawner(spawner.clone());
    
    // Build database with Embassy runtime
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<SensorReading>(|reg| {
        // Configure buffer using Embassy extension trait
        reg.buffer(aimdb_core::buffer::BufferCfg::SingleLatest);
    });
    
    let db = builder.build().unwrap();
    
    // Use database...
    defmt::info!("Database initialized");
}
```

## Task Pool Configuration

The Embassy adapter uses a static task pool for dynamic spawning. Configure the pool size via feature flags:

### Available Pool Sizes

| Feature Flag              | Pool Size | Use Case                |
|---------------------------|-----------|-------------------------|
| `embassy-task-pool-8`     | 8 tasks   | Default, small systems  |
| `embassy-task-pool-16`    | 16 tasks  | Medium complexity       |
| `embassy-task-pool-32`    | 32 tasks  | Complex applications    |

**Important:** Only enable **one** task pool feature at a time.

### Example Configuration

```toml
[dependencies]
aimdb-embassy-adapter = { 
    version = "0.1", 
    default-features = false,
    features = ["embassy-runtime", "embassy-task-pool-16"]
}
```

If you exceed the pool size, you'll get an `ExecutorError::SpawnFailed` error.

## Buffer Configuration

Embassy uses the standard buffer configuration API:

```rust
use aimdb_core::buffer::BufferCfg;

// SPMC Ring buffer
reg.buffer(BufferCfg::SpmcRing { capacity: 8 });

// Single latest value
reg.buffer(BufferCfg::SingleLatest);

// Mailbox (single slot)
reg.buffer(BufferCfg::Mailbox);
```

## Runtime Traits

The adapter implements all required executor traits:

### RuntimeAdapter
```rust
impl RuntimeAdapter for EmbassyAdapter {
    fn runtime_name() -> &'static str {
        "embassy"
    }
}
```

### TimeOps
```rust
impl TimeOps for EmbassyAdapter {
    type Instant = embassy_time::Instant;
    type Duration = embassy_time::Duration;

    fn now(&self) -> Self::Instant {
        embassy_time::Instant::now()
    }
    
    fn millis(&self, ms: u64) -> Self::Duration {
        embassy_time::Duration::from_millis(ms)
    }
    
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        embassy_time::Timer::after(duration)
    }
    
    // ... other methods
}
```

### Logger
```rust
impl Logger for EmbassyAdapter {
    fn info(&self, message: &str) {
        defmt::info!("{}", message);
    }
    
    fn debug(&self, message: &str) {
        defmt::debug!("{}", message);
    }
    
    // ... warn, error via defmt
}
```

### Spawn
```rust
impl Spawn for EmbassyAdapter {
    type SpawnToken = ();
    
    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Uses static task pool
        // Returns () (no handle like Tokio's JoinHandle)
    }
}
```

## Features

```toml
[features]
embassy-runtime = []              # Enable Embassy support
embassy-task-pool-8 = []          # 8-task pool (default)
embassy-task-pool-16 = []         # 16-task pool
embassy-task-pool-32 = []         # 32-task pool
defmt = ["dep:defmt"]             # Embedded logging
```

## Time Operations

Embassy time integration:

```rust
use aimdb_embassy_adapter::EmbassyAdapter;
use embassy_time::Duration;

let adapter = EmbassyAdapter::new().unwrap();

// Get current instant
let now = adapter.now();

// Create duration
let delay = adapter.millis(1000);

// Sleep for duration
adapter.sleep(delay).await;
```

## Logging with Defmt

Efficient embedded logging:

```rust
// In your main.rs or lib.rs
use defmt_rtt as _;  // Transport
use panic_probe as _;  // Panic handler

// Adapter automatically uses defmt
let db = AimDb::build_with(adapter, |builder| {
    // Logs appear via defmt
    builder.configure::<Data>(|reg| {
        // ...
    })
})?;
```

View logs with `probe-rs`:
```bash
probe-rs run --chip STM32F411RETx
```

## Target Architectures

Tested on:
- **ARM Cortex-M**: thumbv6m, thumbv7em, thumbv7m, thumbv8m
- **RISC-V**: riscv32imc, riscv32imac
- **Xtensa**: esp32, esp32s2, esp32s3

## Error Handling

Embassy-specific errors:

```rust
use aimdb_executor::{ExecutorError, ExecutorResult};
use embassy_executor::Spawner;

let adapter = EmbassyAdapter::new_with_spawner(spawner);

// Task pool exhausted
match adapter.spawn(future) {
    Ok(_) => { /* success */ },
    Err(ExecutorError::SpawnFailed { message }) => {
        defmt::error!("Task pool full: {}", message);
        // Increase pool size via feature flag
    },
    Err(ExecutorError::RuntimeUnavailable { message }) => {
        defmt::error!("No spawner: {}", message);
        // Use EmbassyAdapter::new_with_spawner()
    },
    _ => {}
}
```

## Memory Considerations

Embassy environments are memory-constrained:

### Static Allocation
- Use const generics for buffer sizes
- Prefer `heapless` collections
- Avoid dynamic allocation in hot paths

### Stack Usage
- Monitor stack depth in nested async calls
- Use `#[embassy_executor::task]` for spawned tasks

### Flash Usage
- Keep type signatures simple
- Use `defmt` instead of `format!`
- Enable LTO in release builds

## Performance

Optimized for embedded:
- **Zero-copy**: Where possible
- **Static dispatch**: No dynamic allocation
- **Efficient polling**: Embassy's task scheduler
- **Low latency**: Sub-millisecond task switching

## Examples

### Complete Embedded Application

```rust
#![no_std]
#![no_main]

use aimdb_core::AimDbBuilder;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyRecordRegistrarExt};
use embassy_executor::Spawner;
use defmt_rtt as _;
use panic_probe as _;

#[derive(Clone, Debug)]
struct Temperature {
    celsius: f32,
    sensor_id: &'static str,
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    defmt::info!("Starting AimDB on Embassy");
    
    // Initialize hardware
    let p = embassy_stm32::init(Default::default());
    
    // Create adapter with spawner
    let adapter = EmbassyAdapter::new_with_spawner(spawner.clone());
    
    // Build database
    let mut builder = AimDbBuilder::new().runtime(adapter);
    
    builder.configure::<Temperature>(|reg| {
        reg.buffer(aimdb_core::buffer::BufferCfg::SingleLatest);
    });
    
    let db = builder.build().unwrap();
    
    // Start producer/consumer tasks
    // (implementation depends on your use case)
    
    defmt::info!("Database ready");
}
```

## Testing

### Cross-compilation Test
```bash
# Test compilation for embedded target
cargo build -p aimdb-embassy-adapter \
    --target thumbv7em-none-eabihf \
    --no-default-features \
    --features embassy-task-pool-16
```

### Hardware Testing
Use `probe-rs` or `cargo-embed`:
```bash
cargo embed --chip STM32F411RETx --release
```

## Complete Examples

See repository examples:
- `examples/embassy-mqtt-connector-demo` - Embassy with MQTT on RP2040

## Comparison with Tokio Adapter

| Feature              | Embassy Adapter  | Tokio Adapter    |
|----------------------|------------------|------------------|
| Environment          | no_std           | std              |
| Target               | Embedded MCUs    | Servers, desktop |
| Threading            | No               | Yes              |
| Heap allocation      | Limited/None     | Unlimited        |
| Logging              | defmt            | tracing          |
| Task spawn           | Static pool      | Dynamic          |
| SpawnToken type      | `()`             | `JoinHandle`     |
| Instant type         | embassy Instant  | tokio Instant    |
| Duration type        | embassy Duration | tokio Duration   |

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-embassy-adapter --open
```

## License

See [LICENSE](../LICENSE) file.
