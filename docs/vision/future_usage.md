# 🌅 AimDB Future Usage Vision (Illustrative)

Status: NON-FUNCTIONAL EXAMPLE (forward-looking API sketch).  
Purpose: Communicate intended ergonomics & guide incremental implementation.

---

## High-Level Scenario
Edge gateway mirrors sensor stream (MCU → Edge) and republishes aggregated snapshots to cloud consumers with sub-50ms latency, using a unified API across runtimes.

## Goals Demonstrated
- Runtime neutrality (Tokio vs Embassy) via adapters
- Service definition via `#[service]` macro
- Reactive subscription to record changes
- Low-latency publish path using zero-copy views (future)

---

## 1. Service Definition (Runtime Agnostic)
```rust,ignore
use aimdb_core::{DbResult, RuntimeContext};
use aimdb_macros::{service, Record};

/// strongly typed record (future derive macro)
#[derive(Debug, PartialEq, Clone, Record)]
pub struct Sensor {
    temp: f32,
    humidity: f32,
}

/// Periodically samples sensor data and writes to the database
#[service]
pub async fn sensor_poll_service<R: RuntimeAdapter>(ctx: RuntimeContext<R>) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();
    let records = ctx.records();

    log.info("sensor_poll_service:start");
    let interval = time.millis(500);
    loop {
    let reading = acquire_sensor_reading()?; // future: no_std HAL integration
    records.set::<Sensor>(reading.to_sensor_data()).await?; // unified verb: set
    time.sleep(interval).await; // accessor style for sleep
    }
}
```

## 2. Subscription / Reactive Consumer
```rust,ignore

#[derive(Debug, PartialEq, Clone, Record)]
pub struct Derived {
    heat_index: f32,
}

#[service]
pub async fn aggregation_service<R: RuntimeAdapter>(ctx: RuntimeContext<R>) -> DbResult<()> {
    let log = ctx.log();
    let records = ctx.records();
    let metrics = ctx.metrics();

    let mut sub = records.subscribe::<Sensor>().await?; // typed streaming cursor
    while let Some(sensor) = sub.next().await {
        metrics.increment("sensor_updates");
        let derived = Derived { heat_index: calc_heat_index(sensor.temp) };
        records.set::<Derived>(derived).await?;
    }
    Ok(())
}
```

## 3. Application Wiring (Tokio)
```rust,ignore
#[tokio::main]
async fn main() -> DbResult<()> {

    let spec = DatabaseSpec::<TokioAdapter>::builder()
        .record::<Sensor>().max_buffer(10)
        .service(sensor_poll_service).max_subscribers(5)
        .record::<Derived>().max_buffer(5)
        .service(aggregation_service).max_subscribers(5)
        .build();
    let db = new_database(spec)?; // envisioned helper

    // Reactive query
    let mut heat_sub = db.subscribe<Derived>().await?;
    while let Some(v) = heat_sub.next().await { println!("heat index={v}"); }
    Ok(())
}
```

## 4. Application Wiring (Embassy - Pseudocode)
```rust,ignore
#[embassy::main]
async fn main(spawner: Spawner) -> ! {
    // Build spec similarly (pseudocode) – static tasks may be pre-allocated
    let spec = DatabaseSpec::<EmbassyAdapter>::builder()
        .record::<Sensor>().max_buffer(10)
        .service(sensor_poll_service).max_subscribers(5)
        .record::<Derived>().max_buffer(5)
        .service(aggregation_service).max_subscribers(5)
        .build();
    let db = new_database(spawner, spec)?; // envisioned helper

    loop { db.adapter().time().sleep(adapter.millis(1000)).await; }
}
```

## 5. Desired Ergonomic Properties
- `RuntimeContext` unifies: sleep, time, spawn child task, db access, metrics handle.
- Builder pattern for database spec: `Database::builder().with_ring_buffer(64).with_notifications().build()`
- Subscriptions provide backpressure-ready async stream interface.
- Zero-copy `get_view()` for hot path reads (future optimization).

## 6. Open Design Questions (Flagged for Issues)
| Topic | Question | Notes |
|-------|----------|-------|
| Subscription semantics | Bounded vs unbounded channels? | Backpressure strategy TBD |
| Metrics | Built-in vs adapter-provided? | Possibly trait with no_std stub |
| Caching | Where to host sliding windows? | Likely feature-gated module |
| Serialization | pluggable serde vs custom binary | Affects zero-copy design |
| Persistence | WAL / snapshot layering | Out of scope for initial runtime milestone |

## 7. Non-Goals (For This Milestone)
- Persistence durability guarantees
- Cross-node replication protocol
- Security/auth layers
- Full metrics backend integration

---

This document is intentionally aspirational; real APIs will land incrementally. Keep it updated as design decisions solidify.
