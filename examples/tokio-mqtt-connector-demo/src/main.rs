//! MQTT Connector Demo
//!
//! Demonstrates bidirectional MQTT integration with AimDB:
//! - Multiple temperature sensors publishing to different topics
//! - Multiple command consumers receiving from different topics
//!
//! This demo uses `mqtt-connector-demo-common` for the shared contracts, keys,
//! simulated thermometers and monitors, demonstrating AimDB's "write once, run
//! anywhere" capability: this file holds only the setup and the record wiring.
//!
//! ## Wiring
//!
//! The connector is registered with `connect`, which returns a handle. Records
//! take a thermometer as `.input(..)` and link to MQTT with `publish` /
//! `subscribe` on that handle, in the contract's own wire format.
//!
//! ## Compile-Time Safe Keys
//!
//! This demo uses the `#[derive(RecordKey)]` macro to define type-safe
//! record keys. Typos like `SensorKey::TempIndor` are caught at compile time!
//!
//! ## Running
//!
//! Start an MQTT broker:
//! ```bash
//! docker run -d -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
//! ```
//!
//! Run the demo:
//! ```bash
//! cargo run -p tokio-mqtt-connector-demo --features tokio-runtime
//! ```
//!
//! Subscribe to sensor topics:
//! ```bash
//! mosquitto_sub -h localhost -t 'sensors/#' -v
//! ```
//!
//! Send commands:
//! ```bash
//! mosquitto_pub -h localhost -t 'commands/temp/indoor' -m '{"action":"read","sensor_id":"indoor-001"}'
//! ```
//!
//! ## TLS (mqtts://)
//!
//! The broker URL is read from `MQTT_BROKER_URL` (defaults to
//! `mqtt://localhost:1883`). To exercise the `mqtts://` TLS transport against
//! a local mosquitto broker configured with a self-signed CA:
//!
//! ```bash
//! SSL_CERT_FILE=/path/to/ca.crt \
//!     MQTT_BROKER_URL=mqtts://localhost:8883 \
//!     cargo run -p tokio-mqtt-connector-demo --features tokio-runtime
//! ```
//!
//! `SSL_CERT_FILE` is honored by the native-tls/OpenSSL backend on Linux to
//! extend the trusted roots without touching the system store — required
//! because a self-signed broker cert won't otherwise validate.

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, RecordKey};
use aimdb_data_contracts::{publish, subscribe};
use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt, MqttOutboundLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

// Shared contracts, keys, thermometers and monitors from the common crate
use mqtt_connector_demo_common::{
    command_consumer, temperature_logger, CommandKey, SensorKey, SimThermometers, Temperature,
    TemperatureCommand, Thermometer,
};

/// Readings per thermometer before it stops.
const READINGS: usize = 5;

#[tokio::main]
async fn main() -> DbResult<()> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(TokioAdapter::new()?);

    let broker_url =
        std::env::var("MQTT_BROKER_URL").unwrap_or_else(|_| "mqtt://localhost:1883".to_string());

    println!("MQTT Connector Demo");
    println!("===================");
    println!("Broker: {broker_url}");
    println!();

    let mut builder = AimDbBuilder::new().runtime(runtime);
    let mqtt =
        builder.connect(MqttConnector::new(broker_url).with_client_id("tokio-demo-multi-sensor"));
    let SimThermometers {
        indoor,
        outdoor,
        server_room,
    } = SimThermometers::new();

    // Temperature sensors (outbound to MQTT), topics from the keys' `#[link_address]`.
    builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .input(Thermometer::on(indoor).readings(READINGS))
            .tap(temperature_logger)
            .output(
                publish(mqtt, SensorKey::TempIndoor.link_address().unwrap())
                    .with_qos(1)
                    .with_retain(false),
            );
    });

    builder.configure::<Temperature>(SensorKey::TempOutdoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .input(Thermometer::on(outdoor).readings(READINGS))
            .tap(temperature_logger)
            .output(publish(
                mqtt,
                SensorKey::TempOutdoor.link_address().unwrap(),
            ));
    });

    builder.configure::<Temperature>(SensorKey::TempServerRoom, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .input(Thermometer::on(server_room).readings(READINGS))
            .tap(temperature_logger)
            .output(publish(
                mqtt,
                SensorKey::TempServerRoom.link_address().unwrap(),
            ));
    });

    // Command consumers (inbound from MQTT).
    builder.configure::<TemperatureCommand>(CommandKey::TempIndoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .input(subscribe(mqtt, CommandKey::TempIndoor.link_address().unwrap()).with_qos(1))
            .tap(command_consumer);
    });

    builder.configure::<TemperatureCommand>(CommandKey::TempOutdoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .input(subscribe(
                mqtt,
                CommandKey::TempOutdoor.link_address().unwrap(),
            ))
            .tap(command_consumer);
    });

    println!("Subscribe: mosquitto_sub -h localhost -t 'sensors/#' -v");
    println!("Command:   mosquitto_pub -h localhost -t 'commands/temp/indoor' -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    println!();
    println!("Press Ctrl+C to stop.\n");

    builder.run().await
}
