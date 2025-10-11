# 🌅 AimDB Future Usage Vision (Illustrative)

Status: NON-FUNCTIONAL EXAMPLE (forward-looking API sketch).  
Purpose: Communicate intended ergonomics & guide incremental implementation.

---

## High-Level Scenario
Edge gateway mirrors sensor stream (MCU → Edge) and republishes aggregated snapshots to cloud consumers with sub-50ms latency, using a unified API across runtimes.

## Goals
- Runtime neutrality (Tokio vs Embassy) via adapters
- Type-safe record registration with `RecordT` trait
- Producer-consumer pipelines with `Emitter` for cross-record communication
- Protocol connectors (MQTT, Kafka) - planned
- Advanced query/subscription API - planned

---

## 1. Record Definition
```rust
use aimdb_core::{Emitter, RecordRegistrar, RecordT};

/// Strongly typed record
#[derive(Debug, Clone)]
pub struct SensorData {
    pub temp: f32,
    pub humidity: f32,
}

/// Self-registering record with producer-consumer pipeline
impl RecordT for SensorData {
    type Config = (); // Configuration if needed
    
    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, _cfg: &Self::Config) {
        // Register producer - validates/processes incoming data
        reg.producer(|_em, data| async move {
            println!("Sensor reading: temp={:.1}°C, humidity={:.1}%", 
                     data.temp, data.humidity);
        })
        // Register consumers - react to new data
        .consumer(|em, data| async move {
            // Emit derived data to other records
            if data.temp > 30.0 {
                let _ = em.emit(HighTempAlert { temp: data.temp }).await;
            }
        });
    }
}
```

## 2. Runtime-Agnostic Services
```rust
use aimdb_core::{DbResult, RuntimeContext};
use aimdb_executor::Runtime;

/// Background service using runtime context
/// Works with any runtime (Tokio, Embassy, etc.)
pub async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();
    
    log.info("Monitoring service started");
    
    loop {
        let start = time.now();
        
        // Perform monitoring work
        check_system_health().await?;
        
        let elapsed = time.duration_since(time.now(), start);
        log.debug("Health check completed", &[("elapsed_ms", &format!("{:?}", elapsed))]);
        
        time.sleep(time.secs(60)).await;
    }
}

async fn check_system_health() -> DbResult<()> {
    // Implementation
    Ok(())
}
```

## 3. Application Wiring (Tokio - Future Vision)

**Current:** Basic builder with `.record()` and `.build()` works today.  
**Future:** Enhanced with per-record buffer config, subscriptions, and query API.

```rust
use aimdb_core::{Database, DbResult};
use aimdb_tokio_adapter::{TokioAdapter, TokioDatabaseBuilder};

#[tokio::main]
async fn main() -> DbResult<()> {
    // Future: Enhanced builder with buffer configuration
    let db = Database::<TokioAdapter>::builder()
        .record::<SensorData>(&())
            .with_buffer_capacity(2048)  // Future: per-record buffer config
            .with_retention(Duration::from_secs(3600))  // Future: time-based retention
        .record::<DerivedMetrics>(&())
            .with_buffer_capacity(512)
        .record::<HighTempAlert>(&())
        .build()?;
    
    // Get runtime context for spawning services
    let ctx = db.context();
    
    // Spawn background services
    db.spawn(async move {
        monitoring_service(ctx.clone()).await.ok();
    })?;
    
    db.spawn(async move {
        aggregation_service(ctx.clone()).await.ok();
    })?;
    
    // Future: Reactive subscription to record changes
    let mut sub = db.subscribe::<SensorData>().await?;  // Future API
    tokio::spawn(async move {
        while let Some(sensor) = sub.next().await {
            println!("New sensor reading: {:?}", sensor);
        }
    });
    
    // Produce data - triggers producer and consumer pipelines
    db.produce(SensorData { 
        temp: 23.5, 
        humidity: 65.0 
    }).await?;
    
    // Future: Query capabilities
    let recent = db.query::<SensorData>()  // Future API
        .since(Duration::from_secs(300))
        .filter(|s| s.temp > 25.0)
        .collect()
        .await?;
    
    println!("Found {} recent high-temp readings", recent.len());
    
    Ok(())
}
```

## 4. Application Wiring (Embassy - Future Vision)

**Current:** Basic builder with `.record()` and `.build(spawner)` works today.  
**Future:** Enhanced with buffer constraints, overflow strategies, and callback-based observation for no_std.

```rust
use aimdb_core::{Database, DbResult};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyDatabaseBuilder};
use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) -> ! {
    // Future: Enhanced builder with buffer configuration for embedded
    let db = Database::<EmbassyAdapter>::builder()
        .record::<SensorData>(&())
            .with_buffer_capacity(64)  // Future: constrained for embedded
            .with_overflow_strategy(OverflowStrategy::DropOldest)  // Future
        .record::<DerivedMetrics>(&())
            .with_buffer_capacity(32)
        .record::<HighTempAlert>(&())
            .with_buffer_capacity(16)
        .build(spawner);
    
    // Get runtime context
    let ctx = db.context();
    
    // Spawn services on Embassy executor
    db.spawn(async move {
        monitoring_service(ctx.clone()).await.ok();
    }).ok();
    
    db.spawn(async move {
        aggregation_service(ctx.clone()).await.ok();
    }).ok();
    
    // Future: Constrained subscription for embedded (no unbounded streams)
    // Instead, use callback-based approach suitable for no_std
    db.on_change::<SensorData>(|sensor| {  // Future API
        // Process new sensor reading
        defmt::info!("Sensor: temp={}", sensor.temp);
    }).await.ok();
    
    // Main loop with periodic data production
    let mut counter = 0;
    loop {
        let adapter = db.adapter();
        
        // Simulate sensor reading
        db.produce(SensorData {
            temp: 20.0 + (counter % 10) as f32,
            humidity: 50.0,
        }).await.ok();
        
        counter += 1;
        adapter.sleep(adapter.millis(1000)).await;
    }
}
```

## 5. Buffer Configuration (Current)
```rust
use aimdb_core::buffer::BufferCfg;
use aimdb_tokio_adapter::TokioBuffer;

// SPMC Ring Buffer - high-frequency telemetry with multiple consumers
let telemetry_buffer = TokioBuffer::<SensorData>::new(&BufferCfg::SpmcRing {
    capacity: 2048,
});

// SingleLatest - configuration updates, only latest value matters
let config_buffer = TokioBuffer::<Config>::new(&BufferCfg::SingleLatest);

// Mailbox - command processing with overwrite semantics
let command_buffer = TokioBuffer::<Command>::new(&BufferCfg::Mailbox);

// Subscribe to buffer
let reader = telemetry_buffer.subscribe();
while let Ok(data) = reader.recv().await {
    // Process data
}
```

## 6. Current Ergonomic Properties
- ✅ `RuntimeContext` provides: time ops, logging, and accessor pattern
- ✅ Builder pattern for database: `Database::builder().record::<T>(&config).build()`
- ✅ Type-safe producer-consumer with `RecordT` trait
- ✅ Cross-record communication via `Emitter`
- ✅ Pluggable buffers (SPMC Ring, SingleLatest, Mailbox)
- ✅ Call tracking and metrics via `CallStats`
- 🚧 Advanced subscription API (planned)
- 🚧 Zero-copy views (future optimization)

## 7. Future Enhancements (Planned)

### Protocol Connectors (Issues #9-#18)
```rust
// Future: MQTT connector
use aimdb_mqtt_connector::MqttBridge;

let bridge = MqttBridge::builder()
    .broker("mqtt://broker.example.com")
    .topic::<SensorData>("sensors/data")
    .connect(&db)
    .await?;
```

### Advanced Subscription API (Planned)
```rust
// Future: Streaming subscriptions
let mut sub = db.subscribe::<SensorData>().await?;
while let Some(data) = sub.next().await {
    process(data);
}
```

### Query Capabilities (Planned)
```rust
// Future: Time-based queries
let readings = db.query::<SensorData>()
    .since(Duration::from_secs(3600))
    .filter(|r| r.temp > 25.0)
    .collect()
    .await?;
```

## 8. Open Design Questions
| Topic | Status | Notes |
|-------|--------|-------|
| Buffer semantics | ✅ Implemented | Three buffer types working |
| Producer-consumer | ✅ Implemented | Type-safe with emitter |
| Runtime adapters | ✅ Implemented | Tokio and Embassy working |
| Protocol bridges | 🚧 Planned | MQTT, Kafka connectors next |
| Subscription API | 🚧 Planned | Advanced streaming interface |
| Metrics integration | 🚧 Partial | CallStats working, more planned |

## 9. Non-Goals (Current Milestone)
- Persistence/durability (future)
- Cross-node replication (future)
- Built-in security/auth (application layer)
- SQL-like query language (keep it simple)

---

See working examples in `examples/` directory:
- `tokio-runtime-demo` - All buffer types with Tokio
- `embassy-runtime-demo` - Embedded example
- `producer-consumer-demo` - Type-safe patterns with emitter
