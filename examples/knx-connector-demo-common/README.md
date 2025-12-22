# KNX Demo Common

Shared business logic for KNX connector demos that runs identically on **MCU, edge, and cloud**.

## Overview

This crate demonstrates AimDB's core strength: **write once, run anywhere**.

The same monitor functions, data types, and business logic work across all platforms by being generic over the `Runtime` trait. Only platform-specific initialization differs.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                   knx-connector-demo-common                     │
│  ┌─────────────────┐ ┌──────────────────┐ ┌─────────────────┐  │
│  │   Data Types    │ │ Monitor Functions│ │  KNX Addresses  │  │
│  │ TemperatureReading│ │ temperature_monitor│ │ TEMP_LIVING_ROOM│
│  │ LightState      │ │ light_monitor    │ │ LIGHT_CONTROL   │  │
│  │ LightControl    │ │                  │ │                 │  │
│  └─────────────────┘ └──────────────────┘ └─────────────────┘  │
│           ↑                   ↑                   ↑            │
│      alloc::String    Generic<R: Runtime>    Pure constants    │
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

- `std` - Enable std support (enables serde serialization)
- `alloc` - Enable allocator support (required for String types)
- `defmt` - Enable defmt integration for embedded logging

## Usage

### Tokio (Edge/Cloud)

```rust
use knx_connector_demo_common::{
    TemperatureReading, LightState, LightControl,
    temperature_monitor, light_monitor,
    types::addresses,
};
use aimdb_knx_connector::dpt::Dpt9;

builder.configure::<TemperatureReading>("temp.livingroom", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .tap(temperature_monitor)
       .link_from("knx://9/1/0")
       .with_deserializer(|data: &[u8]| {
           let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
           Ok(TemperatureReading::new("Living Room", celsius))
       })
       .finish();
});
```

### Embassy (MCU)

```rust
use knx_connector_demo_common::{
    TemperatureReading, LightState, LightControl,
    temperature_monitor, light_monitor,
};
use aimdb_knx_connector::dpt::Dpt9;

// Same business logic, different buffer configuration
builder.configure::<TemperatureReading>("temp.livingroom", |reg| {
    reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
       .tap(temperature_monitor)  // Same monitor works!
       .link_from("knx://9/1/0")
       .with_deserializer(|data: &[u8]| {
           let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
           Ok(TemperatureReading::new("Living Room", celsius))
       })
       .finish();
});
```

## Data Types

### `TemperatureReading` (DPT 9.001)
Temperature sensor reading with location identifier and Celsius value.

### `LightState` (DPT 1.001 - Inbound)
Light switch state received from KNX bus with group address.

### `LightControl` (DPT 1.001 - Outbound)
Light control command to send to KNX bus.

## Monitor Functions

Both monitors are generic over `R: Runtime + Logger`:

- `temperature_monitor` - Logs temperature readings with emoji formatting
- `light_monitor` - Logs light state changes

## KNX Addresses

Common group addresses are defined in `types::addresses`:

```rust
// Temperature sensors (DPT 9.001)
pub const TEMP_LIVING_ROOM: &str = "9/1/0";
pub const TEMP_BEDROOM: &str = "9/0/1";
pub const TEMP_KITCHEN: &str = "9/1/2";

// Light status feedback (DPT 1.001)
pub const LIGHT_MAIN_STATUS: &str = "1/0/7";
pub const LIGHT_HALLWAY_STATUS: &str = "1/0/8";

// Light control (DPT 1.001)
pub const LIGHT_CONTROL: &str = "1/0/6";
```

## Key Benefits

1. **Code Reuse**: Write business logic once, deploy everywhere
2. **Type Safety**: Same types across all platforms
3. **Testing**: Test on desktop, deploy to embedded
4. **Maintenance**: Single source of truth for domain logic

## Platform Differences

| Aspect | MCU (Embassy) | Edge/Cloud (Tokio) |
|--------|---------------|-------------------|
| Buffer Config | `buffer_sized::<CAP, CONSUMERS>()` | `buffer(BufferCfg::...)` |
| Allocation | `alloc` crate | std heap |
| Logging | defmt | tracing/println |
| Serialization | Not available | serde (with `std` feature) |

## Related Crates

- `tokio-knx-connector-demo` - Full Tokio example using this crate
- `embassy-knx-connector-demo` - Full Embassy example using this crate
- `aimdb-knx-connector` - KNX connector implementation
