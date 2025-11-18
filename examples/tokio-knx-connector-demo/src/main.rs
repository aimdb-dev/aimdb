//! KNX Connector Demo - Bidirectional Bus Monitor
//!
//! Demonstrates bidirectional KNX integration with AimDB:
//! - Inbound: Monitor KNX bus telegrams → process in AimDB
//! - Outbound: Control KNX devices from AimDB (toggle light on key press)
//! - Real-time logging of light switches and temperature sensors
//!
//! ## Running
//!
//! Prerequisites:
//! - KNX/IP gateway on your network (e.g., SCN-IP000.03, Gira X1, ABB IP Interface)
//! - KNX devices configured with group addresses
//!
//! Run the demo:
//! ```bash
//! cargo run --example tokio-knx-demo --features tokio-runtime,tracing
//! ```
//!
//! ## Configuration
//!
//! Update the gateway URL and group addresses in main() to match your setup:
//! - Gateway: "knx://192.168.1.19:3671"
//! - Light switch (monitor): "knx://1/0/7"
//! - Light control (publish): "knx://1/0/6"
//! - Temperature sensor: "knx://9/1/0"

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LightState {
    group_address: String,
    is_on: bool,
    timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temperature {
    group_address: String,
    celsius: f32,
    timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LightControl {
    group_address: String,
    is_on: bool,
    timestamp: u64,
}

/// Input handler that toggles light on key press
async fn input_handler(
    _ctx: RuntimeContext<TokioAdapter>,
    producer: Producer<LightControl, TokioAdapter>,
) {
    println!("\n⌨️  Input handler started. Press ENTER to toggle light on 1/0/6");
    println!("   (This sends GroupValueWrite to the KNX bus)\n");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    let mut light_on = false;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Toggle light state
                light_on = !light_on;

                let state = LightControl {
                    group_address: "1/0/6".to_string(),
                    is_on: light_on,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                };

                match producer.produce(state).await {
                    Ok(_) => {
                        println!(
                            "✅ Published to KNX: 1/0/6 = {} (sent to bus)",
                            if light_on { "ON ✨" } else { "OFF" }
                        );
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to publish: {:?}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error reading input: {}", e);
                break;
            }
        }
    }
}

/// Consumer that logs incoming KNX light telegrams
async fn light_monitor(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<LightState, TokioAdapter>,
) {
    let log = ctx.log();

    log.info("👀 Light monitor started - watching KNX bus...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to light buffer");
        return;
    };

    while let Ok(state) = reader.recv().await {
        log.info(&format!(
            "🔵 KNX telegram received: {} = {}",
            state.group_address,
            if state.is_on { "ON ✨" } else { "OFF" }
        ));
    }
}

/// Consumer that logs incoming KNX temperature telegrams
async fn temperature_monitor(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();

    log.info("🌡️  Temperature monitor started - watching KNX bus...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to temperature buffer");
        return;
    };

    while let Ok(temp) = reader.recv().await {
        log.info(&format!(
            "🌡️  KNX temperature: {} = {:.1}°C",
            temp.group_address, temp.celsius
        ));
    }
}

#[tokio::main]
async fn main() -> DbResult<()> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(TokioAdapter::new()?);

    println!("🔧 Creating database with KNX bus monitor...");
    println!("⚠️  NOTE: Update gateway URL and group addresses to match your setup!\n");

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_knx_connector::KnxConnector::new("knx://192.168.1.19:3671"),
    );

    // Configure LightState record (inbound monitoring only)
    builder.configure::<LightState>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(light_monitor)
            // Subscribe from KNX group address 1/0/7 (inbound)
            .link_from("knx://1/0/7")
            .with_deserializer(|data: &[u8]| {
                let is_on = data.first().map(|&b| b != 0).unwrap_or(false);
                Ok(LightState {
                    group_address: "1/0/7".to_string(),
                    is_on,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                })
            })
            .finish();
    });

    // Configure Temperature record (inbound only - monitoring)
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(temperature_monitor)
            // Subscribe from KNX temperature sensor (group address 9/1/0)
            .link_from("knx://9/1/0")
            .with_deserializer(|data: &[u8]| {
                // DPT 9.001 - 2-byte float temperature
                let celsius = if data.len() >= 4 {
                    // Full frame: TPCI + APCI + 2 data bytes
                    let temp_bytes = [data[2], data[3]];
                    let raw = u16::from_be_bytes(temp_bytes);

                    // DPT 9.001 format:
                    // Bit 15: Sign (0=positive, 1=negative)
                    // Bits 14-11: Exponent (4 bits, unsigned)
                    // Bits 10-0: Mantissa (11 bits, unsigned)
                    // Formula: value = (0.01 * mantissa) * 2^exponent * (sign ? -1 : 1)
                    let sign_bit = (raw >> 15) & 0x01;
                    let exponent = ((raw >> 11) & 0x0F) as i32;
                    let mantissa = (raw & 0x07FF) as i16;

                    // Apply sign
                    let signed_mantissa = if sign_bit == 1 { -mantissa } else { mantissa };

                    // Calculate temperature
                    (0.01 * signed_mantissa as f32) * 2f32.powi(exponent)
                } else if data.len() == 3 {
                    // Standard frame: TPCI + APCI + 2 data bytes (3 bytes total with correct NPDU length)
                    let temp_bytes = [data[1], data[2]];
                    let raw = u16::from_be_bytes(temp_bytes);
                    let sign_bit = (raw >> 15) & 0x01;
                    let exponent = ((raw >> 11) & 0x0F) as i32;
                    let mantissa = (raw & 0x07FF) as i16;
                    let signed_mantissa = if sign_bit == 1 { -mantissa } else { mantissa };
                    (0.01 * signed_mantissa as f32) * 2f32.powi(exponent)
                } else if data.len() == 2 {
                    // Just the temperature bytes (no TPCI/APCI)
                    let raw = u16::from_be_bytes([data[0], data[1]]);
                    let sign_bit = (raw >> 15) & 0x01;
                    let exponent = ((raw >> 11) & 0x0F) as i32;
                    let mantissa = (raw & 0x07FF) as i16;
                    let signed_mantissa = if sign_bit == 1 { -mantissa } else { mantissa };
                    (0.01 * signed_mantissa as f32) * 2f32.powi(exponent)
                } else {
                    0.0
                };

                Ok(Temperature {
                    group_address: "9/1/0".to_string(),
                    celsius,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                })
            })
            .finish();
    });

    // Configure LightControl record (outbound - control KNX device)
    builder.configure::<LightControl>(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(input_handler)
            // Publish to KNX group address 1/0/6 (outbound)
            .link_to("knx://1/0/6")
            .with_serializer(|state: &LightControl| {
                // DPT 1.001 - boolean (1 byte)
                Ok(vec![if state.is_on { 0x01 } else { 0x00 }])
            })
            .finish();
    });

    println!("✅ Database configured with bidirectional KNX integration:");
    println!("   INBOUND (KNX → AimDB):");
    println!("     - knx://1/0/7 (light monitoring, DPT 1.001)");
    println!("     - knx://9/1/0 (temperature monitoring, DPT 9.001)");
    println!("   OUTBOUND (AimDB → KNX):");
    println!("     - knx://1/0/6 (light control, DPT 1.001)");
    println!("   Gateway: 192.168.1.19:3671");
    println!("\n💡 The demo will:");
    println!("   1. Connect to the KNX/IP gateway");
    println!("   2. Monitor KNX bus for telegrams on 1/0/7 and 9/1/0");
    println!("   3. Control light on 1/0/6 when you press ENTER");
    println!("   4. Log all KNX activity in real-time");
    println!("\n   Trigger events by:");
    println!("   - Pressing ENTER to toggle light (1/0/6)");
    println!("   - Pressing physical KNX switches");
    println!("   - Sending telegrams via ETS");
    println!("\n   Press Ctrl+C to stop.\n");

    builder.run().await
}
