# aimdb-knx-connector

KNX/IP connector for AimDB - enables bidirectional communication with KNX building automation networks.

## Installation

⚠️ **Important**: This connector currently requires a patched version of `knx-pico` with critical bug fixes that are not yet published to crates.io.

Add to your `Cargo.toml`:

```toml
[dependencies]
# `std` is the host leg. `critical-section-std-impl` selects the impl the
# connector's channels need to link — only a final binary may pick one.
aimdb-knx-connector = { version = "0.5", features = [
    "std",
    "critical-section-std-impl",
] }
# The host also needs the adapter's UDP socket and clock.
aimdb-tokio-adapter = { version = "0.6", features = ["tokio-runtime", "net"] }

# REQUIRED: Patch knx-pico to use fork with bug fixes
[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }
```

**Why the patch?**
- Fixes DPT 9 (float) encoding/decoding bug
- Prevents panic on malformed telegrams
- Embassy dependency compatibility

We're working with upstream to get these changes merged. Once published, the patch won't be needed.

## Features

- **Runtime-Neutral**: One connector for every runtime — you pass an adapter's
  UDP binder and clock, so no feature here names an executor
- **KNXnet/IP Tunneling**: Full protocol support via UDP port 3671
- **Bidirectional Communication**: Monitor bus activity and send commands
- **Type-Safe Records**: KNX telegrams become strongly-typed Rust records
- **Automatic Reconnection**: Handles network disruptions gracefully
- **Group Address Support**: 3-level format (main/middle/sub)
- **DPT Conversion**: Built-in support for common data point types via knx-pico

## Quick Start (Tokio)

```rust
use aimdb_knx_connector::{Channels, KnxConnector};
use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
use aimdb_tokio_adapter::TokioAdapter;
use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
struct LightState {
    is_on: bool,
}

// The connector's queues. `'static` because the connection task and the pumps
// are spawned as `'static` futures; a `StaticCell` supplies this on an MCU.
// One pair per connector — do not share it between two `KnxConnector`s.
static CHANNELS: Channels = Channels::new();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AimDbBuilder::new()
        .runtime(TokioAdapter::new()?)
        // The adapter owns the UDP socket and the clock; this crate owns the
        // tunnelling protocol.
        .with_connector(KnxConnector::new(
            TokioNet::udp(Ipv4Addr::UNSPECIFIED),
            TokioDelay,
            "knx://192.168.1.19:3671",
            &CHANNELS,
        ))
        .configure::<LightState>(|reg| {
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

## Quick Start (Embassy)

The same constructor — only the binder and clock change, and the channels come
from a `static` instead of being sized at runtime:

```rust
use aimdb_embassy_adapter::net::{EmbassyDelay, EmbassyNet};
use aimdb_knx_connector::{Channels, KnxConnector};

static CHANNELS: Channels<32> = Channels::new();

let binder = EmbassyNet::udp(stack, rx_meta, rx_buf, tx_meta, tx_buf);

let builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(KnxConnector::new(
        binder,
        EmbassyDelay,
        "knx://192.168.1.19:3671",
        &CHANNELS,
    ));
```

Enable `features = ["connector"], default-features = false` — no
`critical-section-std-impl`, since the HAL already provides an impl. See
`examples/embassy-knx-connector-demo/` for the full wiring.

## Group Address Format

Group addresses use 3-level notation: `main/middle/sub`

- **Main**: 0-31 (5 bits)
- **Middle**: 0-7 (3 bits)
- **Sub**: 0-255 (8 bits)

Example: `knx://192.168.1.19:3671/1/0/7`

## DPT Support

Uses `knx-pico` for Data Point Type conversion:

```rust
use knx_pico::dpt::{Dpt1, Dpt5, Dpt9, DptDecode, DptEncode};

// DPT 1.001 - Boolean (switch)
let is_on = Dpt1::Switch.decode(data)?;

// DPT 5.001 - 8-bit unsigned (0-100%)
let percentage = Dpt5::Percentage.decode(data)?;

// DPT 9.001 - 2-byte float (temperature)
let temp = Dpt9::Temperature.decode(data)?;
```

## Examples

- `examples/tokio-knx-connector-demo/` - Tokio runtime demo
- `examples/embassy-knx-connector-demo/` - Embassy runtime demo

## Production Readiness

### ✅ Implemented Features

- **Core Protocol**: Full KNXnet/IP Tunneling support (connection, heartbeat, ACK handling)
- **Dual Runtime**: Both Tokio (std) and Embassy (no_std) implementations
- **ACK Validation**: Outbound telegrams are confirmed with 3-second timeout
- **Auto-Reconnection**: Automatic reconnection on network failures (5s retry interval)
- **Type Safety**: Router-based dispatch with strong typing
- **Tested**: Unit tests for parsing, frame building, and connection state

### ⚠️ Known Limitations

1. **Single Connection Per Gateway**
   - Each connector instance maintains ONE tunnel connection
   - Most KNX gateways support 4-5 concurrent tunnels
   - For multiple connections, create multiple connector instances

2. **No Group Address Discovery**
   - You must manually configure group addresses
   - No automatic ETS project import
   - No runtime discovery of available addresses

3. **Limited DPT Support**
   - Uses external `knx-pico` crate for DPT conversion
   - You must implement custom serializers/deserializers
   - Common DPTs (1.001, 5.001, 9.001) require manual encoding

4. **No KNX Secure Support**
   - No encrypted tunneling (KNX Data Secure, KNX IP Secure)
   - Only plaintext KNXnet/IP supported
   - Use network-level security (VPN, VLANs) instead

5. **No Routing Mode**
   - Only Tunneling mode supported
   - No multicast ROUTING_INDICATION support
   - Cannot act as KNX router

6. **Fire-and-Forget Publishing**
   - Outbound telegrams are sent without application-level confirmation
   - ACK only confirms gateway receipt, not bus delivery
   - No read/response request support (only write operations)

7. **Fixed Reconnection Strategy**
   - 5-second fixed delay between reconnection attempts
   - No exponential backoff
   - No configurable retry limits

### 🔧 Deployment Recommendations

**Network Requirements:**
- Low latency network (< 10ms RTT to gateway preferred)
- Stable connection (reconnection causes 5s service interruption)
- Gateway should support at least 50 telegrams/second

**Gateway Configuration:**
- Enable KNXnet/IP Tunneling on gateway
- Ensure gateway firmware is up-to-date
- Monitor gateway connection limits (typically 4-5 tunnels)

**Resource Requirements:**
- **Tokio**: Minimal (< 1MB heap, negligible CPU)
- **Embassy**: ~32KB heap for buffers, 1-2 tasks

**Monitoring:**
- Watch for "ACK timeout" warnings in logs (indicates network issues)
- Monitor reconnection frequency (should be rare in stable environment)
- Check for "Router dispatch failed" errors (indicates configuration issues)

**Testing Before Production:**
```bash
# 1. Test connectivity
cargo run --example tokio-knx-connector-demo

# 2. Monitor for ACK timeouts (press ENTER multiple times)
# 3. Test reconnection (disconnect network cable briefly)
# 4. Verify group addresses match your KNX installation
```

### 🐛 Troubleshooting

**Connection Failures:**
- Verify gateway IP and port (default: 3671)
- Check firewall rules (UDP port 3671)
- Ensure gateway is not at connection limit
- Try pinging gateway to verify network connectivity

**ACK Timeouts:**
- Check network latency to gateway (should be < 50ms)
- Verify gateway is not overloaded
- Reduce telegram sending rate
- Consider upgrading gateway hardware

**Telegrams Not Received:**
- Verify group address format (main/middle/sub)
- Check that addresses are correct in ETS project
- Enable tracing logs to see raw telegrams
- Use ETS Bus Monitor to verify telegrams are on bus

**Parsing Errors:**
- Check DPT serializer/deserializer implementations
- Verify telegram data length matches DPT specification
- Enable tracing to see raw telegram bytes

### 📊 Performance Characteristics

- **Latency**: Typically 10-30ms from bus event to AimDB record update
- **Throughput**: Tested up to 100 telegrams/second (gateway dependent)
- **ACK Timeout**: 3 seconds (not configurable)
- **Reconnection Delay**: 5 seconds (not configurable)
- **Heartbeat Interval**: 55 seconds (per KNX specification)

### 🔐 Security Considerations

- **No Encryption**: All KNX traffic is plaintext
- **Network Isolation**: Deploy on isolated VLAN or VPN
- **Access Control**: Restrict access to gateway IP
- **Input Validation**: All telegram parsing includes bounds checking
- **No Authentication**: KNXnet/IP has no built-in authentication

### 🚀 Upgrade Path

**From Development to Production:**
1. ✅ Implement ACK handling (completed)
2. ✅ Add comprehensive tests (completed)
3. ⚠️ Add metrics/monitoring (optional, recommended for Phase 2)
4. ⚠️ Add graceful shutdown (optional)
5. ⚠️ Add configurable timeouts (optional)
6. ⚠️ Add KNX Secure support (major feature, Phase 3)

## Examples

- `examples/tokio-knx-connector-demo/` - Tokio runtime demo
- `examples/embassy-knx-connector-demo/` - Embassy runtime demo

## Protocol Details

Implements KNXnet/IP Tunneling:
- Connection establishment via CONNECT_REQUEST/RESPONSE
- Data exchange via TUNNELING_REQUEST/ACK
- cEMI frame parsing for group telegrams
- Automatic sequence counter management

## License

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)