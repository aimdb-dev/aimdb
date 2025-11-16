//! KNX Connector Demo - Bus Monitor
//!
//! Demonstrates KNX bus monitoring with AimDB:
//! - Inbound: Monitor KNX bus telegrams → process in AimDB
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
//! - Light switch: "knx://1/0/7"
//! - Temperature sensor: "knx://1/1/10"

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
            // Subscribe from KNX temperature sensor (group address 1/1/10)
            .link_from("knx://1/1/10")
            .with_deserializer(|data: &[u8]| {
                // DPT 9.001 - 2-byte float temperature
                // Simple parsing: combine bytes as i16, then convert to celsius
                let celsius = if data.len() >= 2 {
                    let raw = i16::from_be_bytes([data[0], data[1]]);
                    let exponent = ((raw as u16) >> 11) & 0x0F;
                    let mantissa = (raw as u16) & 0x7FF;
                    let sign = if (raw as u16 & 0x8000) != 0 {
                        -1.0
                    } else {
                        1.0
                    };
                    sign * (mantissa as f32) * 2f32.powi(exponent as i32 - 12) * 0.01
                } else {
                    0.0
                };

                Ok(Temperature {
                    group_address: "1/1/10".to_string(),
                    celsius,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                })
            })
            .finish();
    });

    println!("✅ Database configured with KNX bus monitor:");
    println!("   INBOUND (KNX → AimDB):");
    println!("     - knx://1/0/7 (light monitoring, DPT 1.001)");
    println!("     - knx://1/1/10 (temperature monitoring, DPT 9.001)");
    println!("   Gateway: 192.168.1.19:3671");
    println!("\n💡 The demo will:");
    println!("   1. Connect to the KNX/IP gateway");
    println!("   2. Monitor KNX bus for telegrams on configured addresses");
    println!("   3. Log all received KNX telegrams in real-time");
    println!("\n   Trigger events by:");
    println!("   - Pressing physical KNX switches");
    println!("   - Sending telegrams via ETS");
    println!("\n   Press Ctrl+C to stop.\n");

    builder.run().await
}
