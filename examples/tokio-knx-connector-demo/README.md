# KNX Connector Demo (Tokio)

Demonstrates bidirectional KNX/IP integration with AimDB using the Tokio runtime.

## Features

- **Inbound Monitoring**: Receives KNX bus telegrams and processes them in AimDB
- **Outbound Control**: Sends commands from AimDB to KNX devices
- **DPT Type Conversion**: Examples of DPT 1.001 (boolean) and DPT 9.001 (temperature)
- **Router-based Dispatch**: Automatic routing of group addresses to typed records

## Prerequisites

### Hardware
- KNX/IP gateway on your network (examples):
  - MDT SCN-IP000.03
  - Gira X1
  - ABB IP Interface
  - Weinzierl KNX IP Interface

### Software
- Rust 1.75+
- Access to KNX/IP gateway on your network

## Configuration

Edit `src/main.rs` to match your KNX setup:

```rust
// Gateway URL
.with_connector(aimdb_knx_connector::KnxConnector::new(
    "knx://YOUR_GATEWAY_IP:3671",  // Change to your gateway IP
))

// Group addresses
.link_from("knx://1/0/7")   // Inbound: light switch
.link_to("knx://1/0/6")     // Outbound: light control
.link_from("knx://1/1/10")  // Inbound: temperature sensor
```

### Finding Your Group Addresses

Use ETS (Engineering Tool Software) or your KNX configuration tool to identify:
- Main group (0-31)
- Middle group (0-7)
- Sub group (0-255)

Example: `1/0/7` = main 1, middle 0, sub 7

## Running

```bash
# From workspace root
cd examples/tokio-knx-connector-demo

# Run with tracing enabled
cargo run --features tokio-runtime,tracing

# Run without tracing
cargo run --features tokio-runtime
```

## Expected Output

```
🔧 Creating database with bidirectional KNX connector...
⚠️  NOTE: Update gateway URL and group addresses to match your setup!

✅ Database configured with bidirectional KNX:
   OUTBOUND (AimDB → KNX):
     - knx://1/0/8 (light control, DPT 1.001)
   INBOUND (KNX → AimDB):
     - knx://1/0/7 (light monitoring, DPT 1.001)
     - knx://1/1/10 (temperature monitoring, DPT 9.001)
   Gateway: 192.168.1.19:3671

💡 The demo will:
   1. Monitor KNX bus for telegrams on configured addresses
   2. Toggle light state every 3 seconds (5 times)
   3. Log all received KNX telegrams

   Press Ctrl+C to stop.

✅ KNX connected, channel_id: 42
💡 Starting light controller service...
👀 Light monitor started - watching KNX bus...
🌡️  Temperature monitor started - watching KNX bus...

💡 Light state changed: 1/0/8 → ON
🔵 KNX telegram received: 1/0/7 = ON ✨
🌡️  KNX temperature: 1/1/10 = 21.5°C
...
```

## Testing

### Trigger KNX Events

Use physical KNX devices or a KNX testing tool:

1. **Press a light switch** configured to send to group address `1/0/7`
2. **Adjust temperature sensor** configured to send to group address `1/1/10`

The demo will log incoming telegrams in real-time.

### Monitor KNX Bus

Use ETS Bus Monitor or `knxtool` to verify outbound telegrams:

```bash
# If you have knxtool installed
knxtool busmonitor1 ip:192.168.1.19
```

You should see telegrams sent to group address `1/0/8` every 3 seconds.

## Troubleshooting

### Connection Failed

```
❌ KNX connection failed: Connection refused
```

**Solution**: Verify gateway IP and port (default 3671)

```rust
"knx://192.168.1.19:3671"  // Check this matches your gateway
```

### No Telegrams Received

```
// Silence after connection
```

**Solutions**:
1. Verify group addresses match your ETS configuration
2. Check that KNX devices are active and sending telegrams
3. Enable tracing: `cargo run --features tokio-runtime,tracing`

### Gateway Rejects Connection

```
Connection rejected by gateway, status: 1
```

**Solution**: Gateway may already have maximum connections. Close other KNX clients.

## DPT Type Reference

The demo includes examples of common Data Point Types:

- **DPT 1.001** (Boolean): Light switches, binary sensors
  ```rust
  // Serialize: bool → 1 byte
  Ok(vec![if is_on { 1 } else { 0 }])
  
  // Deserialize: 1 byte → bool
  let is_on = data.first().map(|&b| b != 0).unwrap_or(false);
  ```

- **DPT 9.001** (2-byte float): Temperature sensors
  ```rust
  // Deserialize: 2 bytes → f32 celsius
  let raw = i16::from_be_bytes([data[0], data[1]]);
  let exponent = (raw >> 11) & 0x0F;
  let mantissa = raw & 0x7FF;
  let celsius = mantissa as f32 * 2_f32.powi((exponent - 12) as i32) * 0.01;
  ```

For more DPT types, see the [KNX DPT specification](https://www.knx.org/).

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    AimDB Database                       │
│                                                         │
│  ┌─────────────┐           ┌─────────────┐            │
│  │ LightState  │           │ Temperature │            │
│  │   Record    │           │   Record    │            │
│  └──────┬──────┘           └──────┬──────┘            │
│         │                         │                    │
│         │ link_to/link_from      │ link_from          │
│         ▼                         ▼                    │
│  ┌──────────────────────────────────────┐             │
│  │      KNX Connector (Tokio)           │             │
│  │  - Router-based dispatch             │             │
│  │  - Background connection task        │             │
│  │  - Automatic reconnection            │             │
│  └───────────────┬──────────────────────┘             │
└──────────────────┼──────────────────────────────────────┘
                   │ UDP Socket (3671)
                   ▼
         ┌────────────────────┐
         │  KNX/IP Gateway    │
         │  (192.168.1.19)    │
         └─────────┬──────────┘
                   │ KNX Bus (TP)
                   ▼
    ┌──────┐  ┌──────┐  ┌──────┐
    │Switch│  │Light │  │Sensor│
    │ 1/0/7│  │1/0/8 │  │1/1/10│
    └──────┘  └──────┘  └──────┘
```

## Next Steps

- Modify group addresses to match your installation
- Add more records for additional KNX devices
- Implement complex logic (e.g., automatic scenes)
- Try different DPT types (see knx-pico documentation)
- Integrate with other connectors (MQTT, HTTP, etc.)

## License

Licensed under either of Apache License 2.0 or MIT license at your option.
