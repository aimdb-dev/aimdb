#![no_std]
#![no_main]

//! KNX Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional KNX/IP integration on embedded hardware with AimDB:
//! - Multiple temperature sensors publishing to different records
//! - Multiple light monitors receiving from different addresses
//! - Outbound light control via button press
//!
//! ## Shared Code
//!
//! This demo uses `knx-connector-demo-common` for the records, keys, DPT
//! codecs and monitors, demonstrating AimDB's "write once, run anywhere"
//! capability. The same business logic runs on MCU (Embassy), edge (Tokio),
//! and cloud (Tokio).
//!
//! ## Layout
//!
//! - `board.rs` owns the hardware: heap, clocks, Ethernet, USART3, and the
//!   user button B1 as an owned input (`NucleoInputs { b1 }`), plus the
//!   `PushButton` input that turns it into `ButtonEvent`s.
//! - `main.rs` is the application: the KNX connector and the records, each
//!   linked through the connector's handle (`InboundLink::on` /
//!   `OutboundLink::on`).
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board (or similar with Ethernet)
//! - Ethernet connection to network with KNX/IP gateway
//! - KNX/IP gateway (e.g., MDT SCN-IP000.03, Gira X1, ABB IP Interface)
//!
//! ## Running
//!
//! 1. Ensure KNX/IP gateway is accessible on your network
//!
//! 2. Update KNX_GATEWAY_IP constant below to match your gateway
//!
//! 3. Update group addresses to match your KNX installation
//!
//! 4. Flash to target from this directory (its `.cargo/config.toml` selects
//!    the thumbv8m target and the probe-rs runner):
//! ```bash
//! cargo run
//! ```
//!
//! 5. Trigger KNX events by:
//!    - Pressing USER button (blue button) to toggle light on 1/0/6
//!    - Pressing physical KNX switches
//!    - Sending telegrams via ETS

extern crate alloc;

mod board;

use aimdb_core::remote::SecurityPolicy;
use aimdb_core::{AimDbBuilder, InboundLink, OutboundLink, RecordKey};
use aimdb_embassy_adapter::io::EmbassyUart;
use aimdb_embassy_adapter::net::{EmbassyDelay, EmbassyNet};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use aimdb_knx_connector::connector::KnxConnector;
use aimdb_serial_connector::SerialServer;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::udp::PacketMetadata;
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use board::PushButton;
// Shared records, keys, DPT codecs and monitors from the common crate
use knx_connector_demo_common::{
    LightControl, LightControlKey, LightKey, LightState, TemperatureKey, TemperatureReading, dpt,
    light_monitor, temperature_monitor,
};

// ============================================================================
// KNX CONFIGURATION
// ============================================================================

/// KNX/IP gateway IP address (modify for your network)
const KNX_GATEWAY_IP: &str = "192.168.1.4";

/// KNX/IP gateway port (default: 3671)
const KNX_GATEWAY_PORT: u16 = 3671;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("🚀 Starting Embassy KNX Connector Demo");
    info!("   Using shared types from knx-connector-demo-common");

    let board = board::init(spawner).await;
    let stack = board.stack;
    let mut led = board.led;
    let (serial_tx, serial_rx) = (board.serial_tx, board.serial_rx);
    let board::NucleoInputs { b1 } = board.inputs;

    info!("🔌 Initializing KNX client...");

    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new());

    use alloc::format;
    let gateway_url = format!("knx://{}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);

    // The adapter owns the UDP socket and the clock; the connector owns the
    // tunnelling protocol. Socket buffers are `'static`, as on any MCU.
    static KNX_RX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
    static KNX_RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    static KNX_TX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
    static KNX_TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    let knx_binder = EmbassyNet::udp(
        *stack,
        KNX_RX_META.init([PacketMetadata::EMPTY; 8]),
        KNX_RX_BUF.init([0; 1024]),
        KNX_TX_META.init([PacketMetadata::EMPTY; 8]),
        KNX_TX_BUF.init([0; 1024]),
    );

    // ── AimX-over-serial: serve this db over USART3 (ST-LINK VCP, PD8=TX/PD9=RX) ──
    // A *second* connector alongside KNX. With no extra cabling on a Nucleo-H563ZI
    // it appears on the host as /dev/ttyACM0; read the live records with:
    //   aimdb --features transport-serial \
    //         --connect serial:///dev/ttyACM0?baud=115200 record list
    // defmt logs ride RTT (SWD), so they don't collide with this data UART.
    // Read-only: each record has a single writer (KNX, or the button), so
    // remote `record.set` is refused — peers can list/get/subscribe, not write.
    // The serial server takes no links, so it needs no handle.
    let mut builder = AimDbBuilder::new().runtime(runtime.clone()).with_connector(
        SerialServer::new(EmbassyUart::new(serial_rx, serial_tx))
            .security_policy(SecurityPolicy::read_only()),
    );
    let knx = builder.connect(KnxConnector::new(knx_binder, EmbassyDelay, &gateway_url));

    // ========================================================================
    // TEMPERATURE SENSORS (inbound: KNX → AimDB)
    // Using shared types and monitors from knx-connector-demo-common
    // ========================================================================

    builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Living Room")),
            );
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Bedroom, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::Bedroom.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Bedroom")),
            );
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Kitchen, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .tap(temperature_monitor)
            .input(
                InboundLink::on(knx, TemperatureKey::Kitchen.link_address().unwrap())
                    .with_deserializer(dpt::temperature_at("Kitchen")),
            );
    });

    // ========================================================================
    // LIGHT MONITORS (inbound: KNX → AimDB)
    // Using shared types and monitors from knx-connector-demo-common
    // ========================================================================

    builder.configure::<LightState>(LightKey::Main, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .input(
                InboundLink::on(knx, LightKey::Main.link_address().unwrap())
                    .with_deserializer(dpt::light_state_at("1/0/7")),
            );
    });

    builder.configure::<LightState>(LightKey::Hallway, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .tap(light_monitor)
            .input(
                InboundLink::on(knx, LightKey::Hallway.link_address().unwrap())
                    .with_deserializer(dpt::light_state_at("1/0/8")),
            );
    });

    // ========================================================================
    // LIGHT CONTROL (outbound: AimDB → KNX)
    // Using shared LightControl type from knx-connector-demo-common
    // ========================================================================

    builder.configure::<LightControl>(LightControlKey::Control, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access()
            .input(PushButton::on(b1))
            .output(
                OutboundLink::on(knx, LightControlKey::Control.link_address().unwrap())
                    .with_serializer(dpt::switch),
            );
    });

    info!("✅ Database configured:");
    info!("   INBOUND (KNX → AimDB):");
    info!("     - temp.livingroom  (9/1/0)");
    info!("     - temp.bedroom     (9/0/1)");
    info!("     - temp.kitchen     (9/1/2)");
    info!("     - lights.main      (1/0/7)");
    info!("     - lights.hallway   (1/0/8)");
    info!("   OUTBOUND (AimDB → KNX):");
    info!("     - lights.control   (1/0/6)");
    info!("   Gateway: {}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);
    info!("   SERIAL (read-only AimX over USART3 / ST-LINK VCP):");
    info!(
        "     aimdb --features transport-serial --connect serial:///dev/ttyACM0?baud=115200 record list"
    );
    info!("");
    info!("Press USER button to toggle light (1/0/6)");

    static DB_CELL: StaticCell<aimdb_core::AimDb> = StaticCell::new();
    let (db, db_runner) = builder.build().await.expect("Failed to build database");
    let _db = DB_CELL.init(db);

    info!("✅ Database running");

    // Drive the AimDB runner and LED blink concurrently.
    embassy_futures::join::join(db_runner.run(), async {
        loop {
            led.set_high();
            Timer::after(Duration::from_millis(100)).await;
            led.set_low();
            Timer::after(Duration::from_millis(900)).await;
        }
    })
    .await;
}
