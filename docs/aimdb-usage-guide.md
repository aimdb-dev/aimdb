# AimDB External Usage Guide

This guide shows how to use AimDB in your own projects, whether you're building for standard platforms (Linux, macOS) or embedded systems.

## Runtime Selection

AimDB supports two async runtime environments:

### **Tokio** (Standard Platforms)
For desktops, servers, and cloud deployments. Uses the standard library and provides the full Rust ecosystem.

**Use when:**
- Building for Linux, macOS, or Windows
- Need access to std library features
- Working with existing Tokio-based codebases
- Want maximum performance on platforms with OS support

### **Embassy** (Embedded Systems)
For microcontrollers and no_std environments. Designed for resource-constrained devices.

**Use when:**
- Targeting MCUs (ARM Cortex-M, RISC-V, etc.)
- Working in no_std environments
- Building IoT edge devices
- Need minimal memory footprint and deterministic behavior

**Creating Embassy projects:** See the [Embassy Project Documentation](https://embassy.dev/book/#_starting_a_new_project) for detailed setup instructions.

---

## Quick Start: Tokio (Standard Platforms)

### 1. Create a New Project

```bash
cargo new my-aimdb-project
cd my-aimdb-project
```

### 2. Configure Dependencies (Cargo.toml)

```toml
[package]
name = "my-aimdb-project"
version = "0.1.0"
edition = "2021"

[dependencies]
# AimDB core and Tokio runtime adapter
aimdb-core = "0.3"
aimdb-tokio-adapter = { version = "0.3", features = ["tokio-runtime"] }

# Optional: KNX connector
# aimdb-knx-connector = { version = "0.2", features = ["tokio-runtime"] }

# Optional: MQTT connector
# aimdb-mqtt-connector = { version = "0.3", features = ["tokio-runtime"] }

# Optional: Sync API wrapper (for blocking code)
# aimdb-sync = "0.3"

# Tokio runtime
tokio = { version = "1.0", features = ["full"] }

# CRITICAL: Patch dependencies for compatibility (see notes below)
[patch.crates-io]
# Required if using KNX connector - bug fixes not yet on crates.io
# knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# Not needed for Tokio-only projects (Embassy patches only)
# mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
# mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

### 3. Basic Example (main.rs)

```rust
use aimdb_core::prelude::*;
use aimdb_tokio_adapter::TokioAdapter;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct SensorData {
    temperature: f32,
    timestamp: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create Tokio runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);
    
    // Build database
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .configure::<SensorData>("sensor.data", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
            
            // Simulate a temperature sensor reading every second
            .source(|ctx, producer| async move {
                let time = ctx.time();
                loop {
                    let data = SensorData {
                        temperature: 20.0 + (rand::random::<f32>() * 5.0), // 20-25°C
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    };
                    
                    let _ = producer.produce(data).await;
                    time.sleep(time.secs(1)).await;
                }
            })
            
            // React to sensor data changes
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe().unwrap();
                while let Ok(data) = reader.recv().await {
                    println!("🌡️  Temperature: {:.1}°C at {}", 
                             data.temperature, data.timestamp);
                }
            });
        })
        .build().await?;
    
    println!("🚀 AimDB running with Tokio...");
    db.run().await
}
```

### 4. Run Your Project

```bash
cargo run
```

---

## Quick Start: Embassy (Embedded Systems)

For creating new Embassy projects, follow the official **[Embassy Getting Started Guide](https://embassy.dev/book/#_starting_a_new_project)**.

Once you have an Embassy project set up, add AimDB:

### Configure Dependencies (Cargo.toml)

```toml
[dependencies]
# AimDB core and Embassy runtime adapter
aimdb-core = { version = "0.3", default-features = false }
aimdb-embassy-adapter = { version = "0.3", features = ["embassy-runtime", "embassy-task-pool-16"] }

# Optional: KNX connector
# aimdb-knx-connector = { version = "0.2", features = ["embassy-runtime"], default-features = false }

# Optional: MQTT connector
# aimdb-mqtt-connector = { version = "0.3", features = ["embassy-runtime"], default-features = false }

# Embassy runtime (example for RP2040)
embassy-executor = { version = "0.6", features = ["arch-cortex-m", "executor-thread"] }
embassy-time = { version = "0.3" }
embassy-rp = { version = "0.2", features = ["time-driver"] }

# CRITICAL: Patch dependencies for compatibility
[patch.crates-io]
# Required: Embassy fork with latest upstream main
embassy-executor = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-time = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-sync = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-futures = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
# Add other embassy-* crates as needed (embassy-rp, embassy-stm32, etc.)

# Required: KNX connector dependencies
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# Required: MQTT connector dependencies
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

### Basic Example (main.rs)

```rust
#![no_std]
#![no_main]

use aimdb_core::prelude::*;
use aimdb_embassy_adapter::EmbassyAdapter;
use embassy_executor::Spawner;
use embassy_rp as hal;
use embassy_rp::gpio::{Input, Pull};

#[derive(Debug, Clone)]
struct ButtonPress {
    count: u32,
    timestamp: u64,
}

/// Button handler that produces events on button press
async fn button_handler(
    ctx: RuntimeContext<EmbassyAdapter>,
    producer: Producer<ButtonPress, EmbassyAdapter>,
    mut button: Input<'static>,
) {
    let log = ctx.log();
    log.info("🔘 Button handler started - press button to generate events\n");
    
    let mut count = 0;
    
    loop {
        // Wait for button press (assuming active low)
        button.wait_for_falling_edge().await;
        
        // Debounce delay
        embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
        
        count += 1;
        let press = ButtonPress {
            count,
            timestamp: embassy_time::Instant::now().as_micros(),
        };
        
        let _ = producer.produce(press).await;
        
        // Wait for button release
        button.wait_for_rising_edge().await;
        embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = hal::init(Default::default());
    
    // Setup button pin (example: GPIO 15 with pull-up)
    let button = Input::new(p.PIN_15, Pull::Up);
    
    // Create Embassy runtime adapter
    let runtime = EmbassyAdapter::new();
    
    // Build database
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .configure::<ButtonPress>("button.press", |reg| {
            reg.buffer_sized::<1, 1>(BufferType::SingleLatest)
            
            // Use button as data source with context
            .source_with_context(button, button_handler)
            
            // Process button press events
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe().unwrap();
                while let Ok(press) = reader.recv().await {
                    defmt::info!("🔘 Button pressed! Count: {}, Time: {}", 
                                 press.count, press.timestamp);
                }
            });
        })
        .build().await.unwrap();
    
    db.run().await.unwrap()
}
```

**Note:** Embassy examples require target-specific configuration. See [embassy-knx-connector-demo](../examples/embassy-knx-connector-demo) or [embassy-mqtt-connector-demo](../examples/embassy-mqtt-connector-demo) for complete working examples.

---

## Using Connectors

AimDB connectors enable integration with external protocols and systems. All connectors support both Tokio and Embassy runtimes.

---

## Using Connectors

AimDB connectors enable integration with external protocols and systems. All connectors support both Tokio and Embassy runtimes.

### KNX Connector

The KNX connector provides KNX/IP tunneling support for building automation systems.

**Add to Cargo.toml:**
```toml
# For Tokio
aimdb-knx-connector = { version = "0.2", features = ["tokio-runtime"] }

# For Embassy
aimdb-knx-connector = { version = "0.2", features = ["embassy-runtime"], default-features = false }

# REQUIRED PATCH (bug fixes not yet on crates.io)
[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }
```

**Example (Tokio):**
```rust
use aimdb_core::prelude::*;
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_knx_connector::KnxConnector;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct LightState {
    is_on: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(KnxConnector::new("knx://192.168.1.19:3671"))
        .configure::<LightState>("light.state", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
               .link_from("knx://1/0/7")
               .with_deserializer(|data: &[u8]| {
                   let is_on = data.get(0).map(|&b| b != 0).unwrap_or(false);
                   Ok(Box::new(LightState { is_on }))
               })
               .finish();
            
            reg.tap(|_, consumer| async move {
                let mut reader = consumer.subscribe().unwrap();
                while let Ok(state) = reader.recv().await {
                    println!("💡 Light: {}", if state.is_on { "ON" } else { "OFF" });
                }
            });
        })
        .build().await?;
    
    db.run().await
}
```

**See also:**
- [tokio-knx-connector-demo](../examples/tokio-knx-connector-demo) - Full Tokio example
- [embassy-knx-connector-demo](../examples/embassy-knx-connector-demo) - Full Embassy example

### MQTT Connector

The MQTT connector enables pub/sub messaging with MQTT brokers.

**Add to Cargo.toml:**
```toml
# For Tokio
aimdb-mqtt-connector = { version = "0.3", features = ["tokio-runtime"] }

# For Embassy
aimdb-mqtt-connector = { version = "0.3", features = ["embassy-runtime"], default-features = false }

# REQUIRED PATCH for Embassy (version compatibility)
[patch.crates-io]
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

**Example (Tokio):**
```rust
use aimdb_core::prelude::*;
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_mqtt_connector::MqttConnector;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Deserialize)]
struct SensorReading {
    temperature: f32,
    humidity: f32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    
    let db = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(MqttConnector::new("mqtt://broker.example.com:1883"))
        .configure::<SensorReading>("sensor.reading", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
               .link_from("mqtt://sensor/room1")
               .with_deserializer(|data: &[u8]| {
                   let reading: SensorReading = serde_json::from_slice(data)?;
                   Ok(Box::new(reading))
               })
               .finish();
            
            reg.tap(|_, consumer| async move {
                let mut reader = consumer.subscribe().unwrap();
                while let Ok(reading) = reader.recv().await {
                    println!("📊 Temp: {:.1}°C, Humidity: {:.1}%", 
                             reading.temperature, reading.humidity);
                }
            });
        })
        .build().await?;
    
    db.run().await
}
```

**See also:**
- [tokio-mqtt-connector-demo](../examples/tokio-mqtt-connector-demo) - Full Tokio example
- [embassy-mqtt-connector-demo](../examples/embassy-mqtt-connector-demo) - Full Embassy example

---

## Current Status & Limitations

### ✅ Ready for Use
- **aimdb-core**: Core database engine with all buffer types
- **aimdb-tokio-adapter**: Tokio runtime support (std)
- **aimdb-embassy-adapter**: Embassy runtime support (embedded)
- **aimdb-knx-connector**: KNX/IP tunneling (both runtimes)
- **aimdb-mqtt-connector**: MQTT (both runtimes)
- **aimdb-sync**: Synchronous API wrapper
- **aimdb-client**: Remote database access (AimX protocol)

### 🔧 Embassy Runtime Requires Patch
The Embassy adapter depends on a forked version of Embassy tracking the latest upstream main. You **must** include these patches for Embassy projects:

```toml
[patch.crates-io]
embassy-executor = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-time = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-sync = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-futures = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
# Add other embassy-* crates as needed (embassy-rp, embassy-stm32, etc.)
```

**Why this is needed:**
- Tracks latest Embassy upstream main branch
- Ensures compatibility with AimDB's Embassy adapter
- Includes latest bug fixes and improvements from Embassy project

**Note:** Tokio-only projects do not need Embassy patches.

### 🔧 KNX Connector Requires Patch
The KNX connector depends on a forked version of `knx-pico` with critical bug fixes. You **must** include this patch:

```toml
[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }
```

**Bug fixes in the fork:**
1. **DPT 9 (float) encoding/decoding**: Fixes incorrect temperature values
2. **Panic prevention**: Handles malformed telegrams with npdu_length=1
3. **Embassy compatibility**: Uses workspace-local embassy checkout

### 🔧 MQTT Connector Requires Patch (Embassy Only)
For Embassy runtime support, the MQTT connector requires patched `mountain-mqtt` dependencies:

```toml
[patch.crates-io]
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

**Why?**
- Embassy dependency version compatibility with our workspace
- Optional for Tokio-only usage, but recommended for consistency

## Issue Reporting

We encourage you to test AimDB and report issues!

### Where to Report
- **AimDB issues**: https://github.com/aimdb-dev/aimdb/issues
- **KNX connector issues**: https://github.com/aimdb-dev/aimdb/issues (label: `knx-connector`)
- **knx-pico issues**: https://github.com/cc90202/knx-pico/issues (upstream)

### What to Include
1. **Minimal reproduction**: Small, self-contained example
2. **Environment**: OS, Rust version (`rustc --version`)
3. **Hardware**: If using embedded (board model, firmware version)
4. **KNX setup**: Gateway model, firmware version, network topology
5. **Logs**: Enable tracing with `RUST_LOG=debug`

### Common Issues

**"Failed to connect to KNX gateway"**
- Check gateway IP and port (default: 3671)
- Verify UDP port 3671 is not blocked by firewall
- Ensure gateway has available tunnel slots (typically 4-5 max)
- Try `ping <gateway-ip>` to verify network connectivity

**"ACK timeout" warnings**
- Network latency too high (>50ms)
- Gateway overloaded
- Network congestion
- Solution: Reduce telegram sending rate or upgrade network

**"Panic on malformed telegram"**
- Make sure you're using the patched knx-pico (see Cargo.toml above)
- Enable debug logs to see raw telegram bytes
- Report the specific telegram that causes panic

**Wrong temperature readings (DPT 9)**
- Make sure you're using the patched knx-pico with DPT 9 bug fix
- Verify you're using the correct DPT for your sensor
- Check telegram bytes with ETS Bus Monitor

## Feature Requests

We're actively developing AimDB. Current priorities:
1. ✅ KNX connector (done)
2. ✅ MQTT connector (done)
3. 🚧 Kafka connector (in progress)
4. 📋 DDS connector (planned)
5. 📋 Publish to crates.io (planned)

Want something else? Open an issue with the `enhancement` label!

## Version Pinning

For maximum stability, you can pin to specific versions on crates.io:

```toml
[dependencies]
aimdb-core = "=0.3.0"
aimdb-tokio-adapter = "=0.3.0"
aimdb-mqtt-connector = "=0.3.0"
aimdb-knx-connector = "=0.2.0"
```

Or use standard semver:
```toml
[dependencies]
aimdb-core = "0.3"  # Will use latest 0.3.x
aimdb-knx-connector = "0.2"  # Will use latest 0.2.x
```

## Migration Path

As bug fixes are upstreamed and published, the patches can be removed:

```toml
# Current (with patches)
[dependencies]
aimdb-knx-connector = "0.2"

[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# After upstream fixes are published
[dependencies]
aimdb-knx-connector = "0.2"  # or newer version

# [patch.crates-io]  <-- Just delete this section!
```

We'll announce when patches are no longer needed:
- GitHub releases
- README updates
- Discussions forum

Stay tuned! 🚀
