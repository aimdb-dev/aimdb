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

## Contents

- **Records** (`types`): `TemperatureReading`, `LightState`, `LightControl`, and the `ButtonEvent` a toggle input yields (`LightControl: From<ButtonEvent>`).
- **Keys** (`keys`): `TemperatureKey`, `LightKey`, `LightControlKey`, each with its group address in `#[link_address]`.
- **DPT codecs** (`dpt`): `temperature_at(location)`, `light_state_at(group_address)` and `switch`, written once instead of per record.
- **Monitors** (`monitors`): `temperature_monitor`, `light_monitor`.

## Features

- `std` - Enable std support
- `defmt` - Enable defmt integration for embedded logging

## Usage

The wiring is the same on both runtimes; only the buffer differs. A group
telegram carries only the value, so the decoders take what the payload does
not (the location, the group address) and the links use them as
deserializers.

### Tokio (Edge/Cloud)

```rust
use aimdb_core::InboundLink;
use knx_connector_demo_common::{dpt, temperature_monitor, TemperatureKey, TemperatureReading};

let knx = builder.connect(KnxConnector::new(TokioNet::udp(Ipv4Addr::UNSPECIFIED), TokioDelay, gateway_url));

builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .tap(temperature_monitor)
       .input(
           InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
               .with_deserializer(dpt::temperature_at("Living Room")),
       );
});
```

### Embassy (MCU)

```rust
// Same business logic, different buffer configuration
builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
    reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
       .tap(temperature_monitor)  // Same monitor works!
       .input(
           InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
               .with_deserializer(dpt::temperature_at("Living Room")),
       );
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
