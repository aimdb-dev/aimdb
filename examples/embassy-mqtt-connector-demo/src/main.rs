#![no_std]
#![no_main]

//! MQTT Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional MQTT integration with multiple sensors:
//! - Multiple temperature sensors publishing to different topics
//! - Multiple command consumers receiving from different topics
//!
//! This demo uses `mqtt-connector-demo-common` for the shared contracts, keys,
//! simulated thermometers and monitors, demonstrating AimDB's "write once, run
//! anywhere" capability.
//!
//! ## Layout
//!
//! - `board.rs` owns the hardware: heap, clocks, Ethernet, USART3, and the
//!   user button B1 as an owned input (`NucleoInputs { b1 }`), plus the
//!   `PushButton` input that turns it into `ButtonEvent`s.
//! - `main.rs` is the application: the MQTT connector and the records, each
//!   wired with `.input(..)` and `publish` / `subscribe`.
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board (or similar with Ethernet)
//! - Ethernet connection to network with MQTT broker
//!
//! ## Task Pool Requirements
//!
//! This demo spawns multiple concurrent tasks:
//! - 3 temperature producers (indoor, outdoor, server_room)
//! - 3 temperature loggers (tap consumers)
//! - 2 command consumers
//! - 2 MQTT connector tasks (manager + event router)
//! - 3 outbound publisher tasks
//!
//! Total: 13 tasks - requires `embassy-task-pool-16` feature in aimdb-embassy-adapter.
//!
//! ## Running
//!
//! 1. Start the bench broker on a machine the board can reach over the LAN.
//!    It enforces authentication on both listeners, so a CONNECT that lost its
//!    credentials is refused rather than quietly accepted:
//! ```bash
//! cd ../../dev/mosquitto && ./gen-certs.sh && docker compose up -d
//! ```
//!
//! 2. Put the address and credentials it prints into the constants below.
//!
//! 3. Build and flash from this directory — its `.cargo/config.toml` selects
//!    the thumbv8m target and the probe-rs runner:
//! ```bash
//! cargo run
//! ```
//!
//! 4. Watch the traffic, and send the board a command:
//! ```bash
//! mosquitto_sub -h <broker> -p 1883 -u aimdb -P aimdb-bench -t 'sensors/#' -v
//! mosquitto_pub -h <broker> -p 1883 -u aimdb -P aimdb-bench \
//!     -t commands/temp/indoor -m '{"action":"read","sensor_id":"indoor-001"}'
//! ```
//!
//! ## TLS (`mqtts://`)
//!
//! `--features tls` switches the same demo to port 8883. The dialer resolves
//! the host, `embedded-tls` verifies the broker against the CA compiled in at
//! `ca.der`, and — this board having no RTC — certificate validity is dated by
//! the connector's own SNTP task, so the first handshake waits for a time sync.
//!
//! `gen-certs.sh` writes `ca.der` into this directory. `MQTT_BROKER_HOST` must
//! then be the same string the script was given: it is what the certificate is
//! verified against, and `embedded-tls` reads only `DNS:` SANs (an `IP:` SAN is
//! skipped), which is why the script puts even an IPv4 literal in as one.
//!
//! ```bash
//! cargo run --features tls
//! ```

extern crate alloc;

mod board;

use aimdb_core::remote::SecurityPolicy;
use aimdb_core::{AimDbBuilder, RecordKey};
use aimdb_data_contracts::{publish, subscribe};
use aimdb_embassy_adapter::io::EmbassyUart;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use aimdb_serial_connector::SerialServer;
use defmt::*;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use aimdb_embassy_adapter::net::EmbassyNet;
use aimdb_mqtt_connector::MqttConnector;
#[cfg(feature = "tls")]
use aimdb_mqtt_connector::TlsOptions;

use board::PushButton;
// Shared contracts, keys, thermometers and monitors from the common crate
use mqtt_connector_demo_common::{
    ButtonKey, ButtonPress, CommandKey, SensorKey, SimThermometers, Temperature,
    TemperatureCommand, Thermometer, button_logger, command_consumer, temperature_logger,
};

// ============================================================================
// MQTT CONFIGURATION
// ============================================================================

/// Where the broker is on your network — an IPv4 literal or a DNS name.
///
/// On a `tls` build this is also what the certificate is verified against, so
/// it must match the host `dev/mosquitto/gen-certs.sh` was given.
const MQTT_BROKER_HOST: &str = "192.168.1.10";

/// Broker port: 1883 plain, 8883 TLS.
#[cfg(not(feature = "tls"))]
const MQTT_BROKER_PORT: u16 = 1883;
#[cfg(feature = "tls")]
const MQTT_BROKER_PORT: u16 = 8883;

/// MQTT CONNECT credentials, which travel in the broker URL below.
const MQTT_USERNAME: &str = "aimdb";
const MQTT_PASSWORD: &str = "aimdb-bench";

const MQTT_KEEP_ALIVE: core::time::Duration = core::time::Duration::from_secs(10);

/// The broker's root CA, DER-encoded. `gen-certs.sh` writes it here.
#[cfg(feature = "tls")]
static MQTT_CA_DER: &[u8] = include_bytes!("../ca.der");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("🚀 Starting Embassy MQTT Connector Demo");

    let board = board::init(spawner).await;
    let stack = board.stack;
    let mut led = board.led;
    let (serial_tx, serial_rx) = (board.serial_tx, board.serial_rx);
    #[cfg(feature = "tls")]
    let rng = board.rng;

    info!("🔌 Initializing MQTT client...");

    // Create AimDB database with Embassy adapter
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new());

    // Build the broker URL. The scheme selects the transport; the authority
    // carries the credentials, which both backends read.
    //
    // Nothing un-escapes this string on the way to the CONNECT, so a password
    // needing percent-encoding (`@`, `:`, `/`) belongs in
    // `.with_credentials(..)` on the builder below instead.
    use alloc::format;
    #[cfg(not(feature = "tls"))]
    let scheme = "mqtt";
    #[cfg(feature = "tls")]
    let scheme = "mqtts";
    let broker_url = format!(
        "{}://{}:{}@{}:{}",
        scheme, MQTT_USERNAME, MQTT_PASSWORD, MQTT_BROKER_HOST, MQTT_BROKER_PORT
    );

    // ── AimX-over-serial: serve this db over USART3 (ST-LINK VCP, PD8=TX/PD9=RX) ──
    // A *second* connector alongside MQTT. With no extra cabling on a Nucleo-H563ZI
    // it appears on the host as /dev/ttyACM0; read the live records with:
    //   aimdb --features transport-serial \
    //         --connect serial:///dev/ttyACM0?baud=115200 record list
    // (sensor records are SpmcRing → use `record drain`/`watch`; `get` has no
    // canonical latest). defmt logs ride RTT (SWD), separate from this data UART.

    // Read-only: each record has a single writer (a sensor source, or MQTT for the
    // command records), so remote `record.set` is refused — peers can
    // list/drain/subscribe, not write.
    // Plain `mqtt://`: the adapter owns the socket, so the buffers are the
    // caller's and visible here. The same line on another runtime's adapter
    // needs no change in the connector.
    #[cfg(not(feature = "tls"))]
    let mqtt = {
        static MQTT_RX: StaticCell<[u8; 4096]> = StaticCell::new();
        static MQTT_TX: StaticCell<[u8; 4096]> = StaticCell::new();
        MqttConnector::new(&broker_url)
            .transport(EmbassyNet::tcp(
                *stack,
                MQTT_RX.init([0; 4096]),
                MQTT_TX.init([0; 4096]),
            ))
            .with_client_id("embassy-demo-001")
            .with_keep_alive(MQTT_KEEP_ALIVE)
    };

    // `mqtts://` dials through the same transport as `mqtt://`; the adapter
    // resolves the host. The board's TRNG, the broker's root CA, and the record
    // buffers (16 640 bytes read is the enforced minimum — a TLS 1.3 peer may
    // send full-size records). `init_with` keeps the arrays off the stack.
    // This board has no RTC, so the validity clock comes from SNTP.
    #[cfg(feature = "tls")]
    let mqtt = {
        static MQTT_RX: StaticCell<[u8; 4096]> = StaticCell::new();
        static MQTT_TX: StaticCell<[u8; 4096]> = StaticCell::new();
        static TLS_READ_BUF: StaticCell<[u8; 16_640]> = StaticCell::new();
        static TLS_WRITE_BUF: StaticCell<[u8; 4_096]> = StaticCell::new();
        MqttConnector::new(&broker_url)
            .tls(
                EmbassyNet::tcp(*stack, MQTT_RX.init([0; 4096]), MQTT_TX.init([0; 4096])),
                TlsOptions::new(
                    rng,
                    MQTT_CA_DER,
                    TLS_READ_BUF.init_with(|| [0; 16_640]),
                    TLS_WRITE_BUF.init_with(|| [0; 4_096]),
                )
                .with_sntp(stack, "pool.ntp.org"),
            )
            .with_client_id("embassy-demo-001")
            .with_keep_alive(MQTT_KEEP_ALIVE)
    };

    // The serial server takes no links, so it needs no handle.
    let mut builder = AimDbBuilder::new().runtime(runtime.clone()).with_connector(
        SerialServer::new(EmbassyUart::new(serial_rx, serial_tx))
            .security_policy(SecurityPolicy::read_only()),
    );
    let mqtt = builder.connect(mqtt);
    let board::NucleoInputs { b1 } = board.inputs;

    // The simulated thermometers cycle their readings endlessly: reconnect,
    // re-subscribe and ping cadence only become observable while something
    // is still publishing.
    let SimThermometers {
        indoor,
        outdoor,
        server_room,
    } = SimThermometers::new();

    // ========================================================================
    // TEMPERATURE SENSORS (outbound: AimDB → MQTT)
    // Using compile-time safe SensorKey enum - typos caught at compile time!
    // ========================================================================

    // Three subscriber slots per sensor: the logger tap, the MQTT link and one
    // remote-access subscriber (`aimdb watch` over the serial port). With two,
    // a remote subscribe finds no slot left.
    builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
        reg.buffer_sized::<16, 3>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .input(Thermometer::on(indoor))
            .tap(temperature_logger)
            .output(publish(mqtt, SensorKey::TempIndoor.link_address().unwrap()));
    });

    builder.configure::<Temperature>(SensorKey::TempOutdoor, |reg| {
        reg.buffer_sized::<16, 3>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .input(Thermometer::on(outdoor))
            .tap(temperature_logger)
            .output(publish(
                mqtt,
                SensorKey::TempOutdoor.link_address().unwrap(),
            ));
    });

    builder.configure::<Temperature>(SensorKey::TempServerRoom, |reg| {
        reg.buffer_sized::<16, 3>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .input(Thermometer::on(server_room))
            .tap(temperature_logger)
            .output(publish(
                mqtt,
                SensorKey::TempServerRoom.link_address().unwrap(),
            ));
    });

    // ========================================================================
    // USER BUTTON B1 (outbound: AimDB → MQTT)
    // ========================================================================

    builder.configure::<ButtonPress>(ButtonKey::B1, |reg| {
        reg.buffer_sized::<8, 3>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .input(PushButton::on(b1))
            .tap(button_logger)
            .output(publish(mqtt, ButtonKey::B1.link_address().unwrap()));
    });

    // ========================================================================
    // COMMAND CONSUMERS (inbound: MQTT → AimDB)
    // Using compile-time safe CommandKey enum
    // ========================================================================

    builder.configure::<TemperatureCommand>(CommandKey::TempIndoor, |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .tap(command_consumer)
            .input(subscribe(
                mqtt,
                CommandKey::TempIndoor.link_address().unwrap(),
            ));
    });

    builder.configure::<TemperatureCommand>(CommandKey::TempOutdoor, |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .tap(command_consumer)
            .input(subscribe(
                mqtt,
                CommandKey::TempOutdoor.link_address().unwrap(),
            ));
    });

    info!("✅ Database configured with multi-sensor MQTT:");
    info!("   OUTBOUND: sensors/temp/indoor, outdoor, server_room");
    info!("             sensors/button/b1 (press the blue user button)");
    info!("   INBOUND:  commands/temp/indoor, outdoor");
    // Without the authority: the URL carries the password, and this line goes
    // to the RTT log.
    info!(
        "   Broker:   {}://{}:{}",
        scheme, MQTT_BROKER_HOST, MQTT_BROKER_PORT
    );
    info!("   SERIAL (read-only AimX over USART3 / ST-LINK VCP):");
    info!(
        "     aimdb --features transport-serial --connect serial:///dev/ttyACM0?baud=115200 record list"
    );
    info!("");
    #[cfg(not(feature = "tls"))]
    {
        info!(
            "Subscribe: mosquitto_sub -h {} -u {} -P <password> -t 'sensors/#' -v",
            MQTT_BROKER_HOST, MQTT_USERNAME
        );
        info!(
            "Command:   mosquitto_pub -h {} -u {} -P <password> -t 'commands/temp/indoor' \\",
            MQTT_BROKER_HOST, MQTT_USERNAME
        );
        info!("             -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    }
    #[cfg(feature = "tls")]
    info!("TLS: first connect waits for the automatic SNTP time sync");
    info!("");

    static DB_CELL: StaticCell<aimdb_core::AimDb> = StaticCell::new();
    let (db, db_runner) = builder.build().await.expect("Failed to build database");
    let _db = DB_CELL.init(db);

    info!("✅ Database running with background services");

    // Drive the AimDB runner (all connector/tap/source futures) and LED blink concurrently.
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
