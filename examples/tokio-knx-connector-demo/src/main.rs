//! KNX Connector Demo
//!
//! Demonstrates bidirectional KNX/IP integration with AimDB:
//! - Multiple temperature sensors using the same `TemperatureReading` type
//! - Multiple light monitors using the same `LightState` type
//! - Outbound light control via keyboard input
//!
//! ## Shared Code
//!
//! This demo uses `knx-connector-demo-common` for the records, keys, DPT
//! codecs and monitors, demonstrating AimDB's "write once, run anywhere"
//! capability. The same business logic runs on MCU (Embassy), edge (Tokio),
//! and cloud (Tokio).
//!
//! ## Wiring
//!
//! The KNX connector is registered with `connect`, which returns a handle.
//! Records link through it with `InboundLink::on` / `OutboundLink::on`. The
//! ENTER key is an input that yields `ButtonEvent`s, and `LightControl` is
//! built from them with `From`.
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
use aimdb_core::{
    AimDbBuilder, DbResult, InboundLink, Input, OutboundLink, RecordKey, RecordRegistrar,
};
use aimdb_knx_connector::KnxConnector;
use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use std::fmt::Debug;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

// Shared records, keys, DPT codecs and monitors from the common crate
use knx_connector_demo_common::{
    dpt, light_monitor, temperature_monitor, ButtonEvent, LightControl, LightControlKey, LightKey,
    LightState, TemperatureKey, TemperatureReading,
};

// ============================================================================
// INPUT (platform-specific: stdin only available on std)
// ============================================================================

/// The ENTER key as a push button: each press yields a [`ButtonEvent`], for
/// every record type built `From<ButtonEvent>`.
struct EnterKey;

impl<T> Input<T> for EnterKey
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        reg.source(|ctx, producer| async move {
            let log = ctx.log();
            log.info("\n⌨️  Press ENTER to toggle light on 1/0/6");
            log.info("   (This sends GroupValueWrite to the KNX bus)\n");

            let mut reader = BufReader::new(tokio::io::stdin());
            let mut line = String::new();
            let mut press = 0;
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        press += 1;
                        producer.produce(T::from(ButtonEvent { press }));
                    }
                    Err(e) => {
                        log.error(&format!("Error reading input: {e}"));
                        break;
                    }
                }
            }
        });
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

    // The adapter owns the UDP socket and the clock; the connector owns the
    // tunnelling protocol.
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(UdsServer::from_config(remote_config));
    let knx = builder.connect(KnxConnector::new(
        TokioNet::udp(Ipv4Addr::UNSPECIFIED),
        TokioDelay,
        "knx://192.168.1.4:3671",
    ));

    // Temperature sensors (inbound) - using link_address() from key metadata
    builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Living Room")),
            );
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Bedroom, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::Bedroom.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Bedroom")),
            );
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Kitchen, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::Kitchen.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Kitchen")),
            );
    });

    // Light monitors (inbound) - using link_address() from key metadata
    builder.configure::<LightState>(LightKey::Main, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .input(
                InboundLink::on(knx, LightKey::Main.link_address().unwrap())
                    .with_deserializer(dpt::light_state_at("1/0/7")),
            );
    });

    builder.configure::<LightState>(LightKey::Hallway, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .input(
                InboundLink::on(knx, LightKey::Hallway.link_address().unwrap())
                    .with_deserializer(dpt::light_state_at("1/0/8")),
            );
    });

    // Light control (outbound) - using link_address() from key metadata
    builder.configure::<LightControl>(LightControlKey::Control, |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .input(EnterKey)
            .output(
                OutboundLink::on(knx, LightControlKey::Control.link_address().unwrap())
                    .with_serializer(dpt::switch),
            );
    });

    println!("Press ENTER to toggle light (1/0/6). Press Ctrl+C to stop.\n");
    println!("📡 Remote access (read-only) on {socket_path}");
    println!("   aimdb --connect unix://{socket_path} record list");
    println!("   aimdb --connect unix://{socket_path} watch temp.livingroom\n");

    builder.run().await
}
