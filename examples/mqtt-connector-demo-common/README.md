# MQTT Demo Common

Shared business logic for MQTT connector demos that runs identically on **MCU, edge, and cloud**.

## Overview

This crate demonstrates AimDB's core strength: **write once, run anywhere**.

The same monitor functions, data types, and business logic work across all platforms by being generic over the `RuntimeAdapter` trait. Only platform-specific initialization differs.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                   mqtt-connector-demo-common                    │
│  ┌─────────────────┐ ┌──────────────────┐                      │
│  │   Contracts     │ │ Monitor Functions│                      │
│  │ Temperature     │ │ temperature_logger│                     │
│  │ TemperatureCmd  │ │ command_consumer │                      │
│  └─────────────────┘ └──────────────────┘                      │
│           ↑                   ↑                                │
│    alloc::String      Generic<R: Runtime>                      │
└─────────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
     ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
     │  Embassy    │  │   Tokio     │  │   Tokio     │
     │ STM32/RP2040│  │ Raspberry Pi│  │   Server    │
     │    (MCU)    │  │   (Edge)    │  │  (Cloud)    │
     └─────────────┘  └─────────────┘  └─────────────┘
```

## Contents

- **Contracts** (`types`): `Temperature`, `TemperatureCommand`, `ButtonPress`. Their wire format is JSON through `#[derive(Linkable)]` on every target, `no_std + alloc` included. `catalog!` lists them, and `cargo test` checks their wire shapes against the committed `aimdb.lock` (`AIMDB_UPDATE_LOCK=1 cargo test` to accept a new contract or version).
- **Keys** (`keys`): `SensorKey`, `CommandKey`, each with its MQTT topic in `#[link_address]`.
- **Simulated thermometers** (`sim`): `SimThermometers` hands out one owned channel per sensor; `Thermometer::on(channel)` is an input for every record type built `From<Reading>`.
- **Monitors** (`monitors`): `temperature_logger`, `command_consumer`.

## Features

- `std` - Enable std support
- `defmt` - Enable defmt integration for embedded logging

## Usage

The wiring is the same on both runtimes; only the buffer differs.

### Tokio (Edge/Cloud)

```rust
use aimdb_data_contracts::publish;
use mqtt_connector_demo_common::{SensorKey, SimThermometers, Temperature, Thermometer, temperature_logger};

let mqtt = builder.connect(MqttConnector::new(broker_url));
let SimThermometers { indoor, .. } = SimThermometers::new();

builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
       .input(Thermometer::on(indoor))
       .tap(temperature_logger)  // Shared monitor
       .output(publish(mqtt, SensorKey::TempIndoor.link_address().unwrap()));
});
```

### Embassy (MCU)

```rust
builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
    reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
       .input(Thermometer::on(indoor))  // Same input works!
       .tap(temperature_logger)         // Same monitor works!
       .output(publish(mqtt, SensorKey::TempIndoor.link_address().unwrap()));
});
```

## Key Benefits

1. **Code Reuse**: Write business logic once, deploy everywhere
2. **Type Safety**: Same types across all platforms
3. **Testing**: Test on desktop, deploy to embedded
4. **Maintenance**: Single source of truth for domain logic

## Related Crates

- `tokio-mqtt-connector-demo` - Full Tokio example using this crate
- `embassy-mqtt-connector-demo` - Full Embassy example using this crate
- `aimdb-mqtt-connector` - MQTT connector implementation
