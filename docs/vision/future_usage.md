# AimDB Future Usage Vision (Weather Station)

Status: NON-FUNCTIONAL EXAMPLE (forward-looking API sketch).
Purpose: Communicate intended ergonomics and guide incremental implementation using the weather-station scenario.

This page summarizes the end-to-end flow without inline code. For concrete examples, see:
- Record definitions, producers, and consumers: `docs/vision/record_definition.rs`
- Synchronous main wiring AimDB on a dedicated thread: `docs/vision/main.rs`

---

## Scenario
An edge gateway ingests weather sensor data (MCU → Edge), processes alerts locally, and (future) republishes to external systems with sub-50ms latency. The same ergonomic API spans std (Tokio) and no_std (Embassy) targets.

## Goals
- Runtime neutrality via adapters (Tokio, Embassy)
- Type-safe records with producer/consumer pipelines
- Cross-record communication (Emitter) and outbox pattern
- Protocol connectors (MQTT, Kafka) – planned
- Advanced subscription and query APIs – planned

## What’s Implemented Today
- Core database engine, runtime adapters, and buffers (SPMC Ring, SingleLatest, Mailbox)
- Type-safe producer-consumer patterns and emitter
- Working examples under `examples/`

See the full weather-station sketch in `docs/vision/record_definition.rs`.

Highlights from that file:
- WeatherReading record with producer task to ingest sensor values
- Consumers for alerting, statistics/logging, and MQTT publishing (conceptual)
- Clear separation of concerns: data capture, local processing, and external IO

## Wiring It Up
A synchronous app can host AimDB (async) on its own thread and interact via simple get/set APIs. See `docs/vision/main.rs` for how the app:
- Builds and registers records (readings and alerts)
- Spawns AimDB in a dedicated thread
- Writes readings via a blocking `set::<T>()` call from the main loop

## Protocol Connectors (Planned)
Future connectors will bridge records to external systems with feature flags:
- MQTT bridge for IoT device connectivity
- Kafka bridge for cloud-scale streaming

Until then, the example uses an explicit consumer task to publish alerts to MQTT (see `docs/vision/record_definition.rs`).

## Subscriptions and Queries (Planned)
- Reactive subscriptions to record changes (streaming API)
- Time-windowed and predicate-based queries for recent data

These are sketched in comments and docs to guide the API direction and will arrive in incremental steps.

## Buffer Choices
Pick the buffer type per record based on access pattern (already implemented):
- SPMC Ring – high-frequency telemetry with multiple consumers
- SingleLatest – config/alerts where only the latest value matters
- Mailbox – command processing with overwrite semantics

Concrete usage and configuration examples live in the examples directory and in the record definition sketch.

## Next Steps
- Implement protocol connector crates (MQTT/Kafka) under feature flags
- Expand subscription/query ergonomics and docs
- Add a quick-start example and CLI tooling for inspection

For full details and runnable patterns, explore:
- `docs/vision/record_definition.rs`
- `docs/vision/main.rs`
- `examples/tokio-runtime-demo/`
- `examples/producer-consumer-demo/`
- `examples/embassy-runtime-demo/`
