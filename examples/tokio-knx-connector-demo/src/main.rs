//! KNX Connector Demo
//!
//! Demonstrates bidirectional KNX/IP integration with AimDB:
//! - Multiple temperature sensors using the same `TemperatureReading` type
//! - Multiple light monitors using the same `LightState` type
//! - Outbound light control via keyboard input
//!
//! ## Shared Code
//!
//! This demo uses `knx-connector-demo-common` for data types and monitors,
//! demonstrating AimDB's "write once, run anywhere" capability. The same
//! business logic runs on MCU (Embassy), edge (Tokio), and cloud (Tokio).
//!
//! ## Running
//!
//! ```bash
//! cargo run -p tokio-knx-connector-demo --features tokio-runtime
//! ```
//!
//! ## Configuration
//!
//! Update the gateway URL and group addresses in `main()` to match your KNX setup.

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::{AimDbBuilder, DbResult, Producer, RecordKey, RuntimeContext};
use aimdb_knx_connector::dpt::{Dpt1, Dpt9, DptDecode, DptEncode};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

// Import shared types, monitors, and keys from common crate
use knx_connector_demo_common::{
    light_monitor, temperature_monitor, LightControl, LightControlKey, LightKey, LightState,
    TemperatureKey, TemperatureReading,
};

// ============================================================================
// HANDLERS (platform-specific: stdin only available on std)
// ============================================================================

/// Keyboard input handler - toggles light on ENTER
async fn input_handler(ctx: RuntimeContext, producer: Producer<LightControl>) {
    let log = ctx.log();
    log.info("\n⌨️  Input handler started. Press ENTER to toggle light on 1/0/6");
    log.info("   (This sends GroupValueWrite to the KNX bus)\n");

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

                let state = LightControl::new("1/0/6", light_on);

                producer.produce(state);
            }
            Err(e) => {
                log.error(&format!("Error reading input: {}", e));
                break;
            }
        }
    }
}

#[tokio::main]
async fn main() -> DbResult<()> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(TokioAdapter::new()?);

    println!("KNX Connector Demo");
    println!("==================");
    println!();
    println!("Using shared types from knx-connector-demo-common");
    println!("⚠️  Update gateway URL and group addresses to match your setup!\n");

    // Expose this database over a Unix-domain socket as a *second* connector
    // alongside KNX (the builder drives any number of them). A host can then
    // introspect it live with the `aimdb` CLI or the aimdb-mcp server:
    //
    //   aimdb --connect unix:///tmp/aimdb-knx.sock record list
    //   aimdb --connect unix:///tmp/aimdb-knx.sock record get temp.livingroom
    //
    // Read-only: KNX owns the writer for every record (single-writer-per-key), so
    // remote `record.set` is disallowed — peers can list/get/subscribe, not write.
    let socket_path = "/tmp/aimdb-knx.sock";
    let _ = std::fs::remove_file(socket_path); // clear a stale socket from a prior run
    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(SecurityPolicy::read_only())
        .max_connections(10);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(aimdb_knx_connector::KnxConnector::new(
            "knx://192.168.1.4:3671",
        ))
        .with_connector(UdsServer::from_config(remote_config));

    // Temperature sensors (inbound) - using link_address() from key metadata
    builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .link_from(TemperatureKey::LivingRoom.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Living Room", celsius))
            })
            .finish();
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Bedroom, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .link_from(TemperatureKey::Bedroom.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Bedroom", celsius))
            })
            .finish();
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Kitchen, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .link_from(TemperatureKey::Kitchen.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Kitchen", celsius))
            })
            .finish();
    });

    // Light monitors (inbound) - using link_address() from key metadata
    builder.configure::<LightState>(LightKey::Main, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .link_from(LightKey::Main.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| {
                let is_on = Dpt1::Switch.decode(data).unwrap_or(false);
                Ok(LightState::new("1/0/7", is_on))
            })
            .finish();
    });

    builder.configure::<LightState>(LightKey::Hallway, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .link_from(LightKey::Hallway.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| {
                let is_on = Dpt1::Switch.decode(data).unwrap_or(false);
                Ok(LightState::new("1/0/8", is_on))
            })
            .finish();
    });

    // Light control (outbound) - using link_address() from key metadata
    builder.configure::<LightControl>(LightControlKey::Control, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .source(input_handler)
            .link_to(LightControlKey::Control.link_address().unwrap())
            .with_serializer(|_ctx, state: &LightControl| {
                let mut buf = [0u8; 1];
                let len = Dpt1::Switch.encode(state.is_on, &mut buf).unwrap_or(0);
                Ok(buf[..len].to_vec())
            })
            .finish();
    });

    println!("Press ENTER to toggle light (1/0/6). Press Ctrl+C to stop.\n");
    println!("📡 Remote access (read-only) on {socket_path}");
    println!("   aimdb --connect unix://{socket_path} record list");
    println!("   aimdb --connect unix://{socket_path} watch temp.livingroom\n");

    builder.run().await
}
