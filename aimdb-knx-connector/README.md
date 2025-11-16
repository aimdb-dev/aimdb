# aimdb-knx-connector

KNX/IP connector for AimDB - enables bidirectional communication with KNX building automation networks.

## Features

- **Dual Runtime Support**: Works with both Tokio (std) and Embassy (no_std) runtimes
- **KNXnet/IP Tunneling**: Full protocol support via UDP port 3671
- **Bidirectional Communication**: Monitor bus activity and send commands
- **Type-Safe Records**: KNX telegrams become strongly-typed Rust records
- **Automatic Reconnection**: Handles network disruptions gracefully
- **Group Address Support**: 3-level format (main/middle/sub)
- **DPT Conversion**: Built-in support for common data point types via knx-pico

## Quick Start (Tokio)

```rust
use aimdb_knx_connector::KnxConnector;
use aimdb_tokio_adapter::TokioAdapter;

#[derive(Debug, Clone)]
struct LightState {
    is_on: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AimDbBuilder::new()
        .runtime(TokioAdapter::new()?)
        .with_connector(KnxConnector::new("knx://192.168.1.19:3671"))
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

See `examples/embassy-knx-demo/` for embedded usage.

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

- `examples/tokio-knx-demo/` - Tokio runtime demo
- `examples/embassy-knx-demo/` - Embassy runtime demo

## Protocol Details

Implements KNXnet/IP Tunneling:
- Connection establishment via CONNECT_REQUEST/RESPONSE
- Data exchange via TUNNELING_REQUEST/ACK
- cEMI frame parsing for group telegrams
- Automatic sequence counter management

## License

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)