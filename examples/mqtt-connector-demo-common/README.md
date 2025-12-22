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
│  │   Data Types    │ │ Monitor Functions│                      │
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

## Features

- `std` - Enable std support (serde_json for serialization)
- `alloc` - Enable allocator support (required for most features)
- `defmt` - Enable defmt integration for embedded logging

## Usage

### Tokio (Edge/Cloud)

```rust
use mqtt_connector_demo_common::{Temperature, TemperatureCommand, temperature_logger, command_consumer};

builder.configure::<Temperature>("sensor.temp.indoor", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
       .tap(temperature_logger)  // Shared monitor
       .link_to("mqtt://sensors/temp/indoor")
       .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
       .finish();
});
```

### Embassy (MCU)

```rust
use mqtt_connector_demo_common::{Temperature, TemperatureCommand, temperature_logger, command_consumer};

builder.configure::<Temperature>("sensor.temp.indoor", |reg| {
    reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
       .tap(temperature_logger)  // Same monitor works!
       .link_to("mqtt://sensors/temp/indoor")
       .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
       .finish();
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
