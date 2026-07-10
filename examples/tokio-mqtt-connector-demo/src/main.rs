//! MQTT Connector Demo
//!
//! Demonstrates bidirectional MQTT integration with AimDB:
//! - Multiple temperature sensors publishing to different topics
//! - Multiple command consumers receiving from different topics
//!
//! This demo uses `mqtt-connector-demo-common` for shared types and monitors,
//! demonstrating AimDB's "write once, run anywhere" capability.
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
use aimdb_core::{AimDbBuilder, DbResult, Producer, RecordKey, RuntimeContext};
use aimdb_data_contracts::LinkableRegistrarExt;
use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

// Import shared types, monitors, and compile-time safe keys from the common crate
use mqtt_connector_demo_common::{
    command_consumer, temperature_logger, CommandKey, SensorKey, Temperature, TemperatureCommand,
};

// ============================================================================
// TEMPERATURE PRODUCERS (platform-specific)
// ============================================================================

/// Indoor temperature sensor producer
async fn indoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🏠 Starting INDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("indoor-001", 22.0 + (i as f32 * 0.5)); // Indoor temps: 22-24°C

        log.info(&format!(
            "🏠 Indoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        time.sleep_secs(2).await;
    }

    log.info("✅ Indoor producer finished");
}

/// Outdoor temperature sensor producer
async fn outdoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🌳 Starting OUTDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("outdoor-001", 5.0 + (i as f32 * 1.0)); // Outdoor temps: 5-9°C (cold!)

        log.info(&format!(
            "🌳 Outdoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        time.sleep_secs(2).await;
    }

    log.info("✅ Outdoor producer finished");
}

/// Server room temperature sensor producer
async fn server_room_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🖥️  Starting SERVER ROOM temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("server-room-001", 18.0 + (i as f32 * 0.2)); // Server room: 18-19°C (cooled)

        log.info(&format!(
            "🖥️  Server room sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        time.sleep_secs(2).await;
    }

    log.info("✅ Server room producer finished");
}

// ============================================================================
// MAIN
// ============================================================================

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

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new(broker_url)
            .with_client_id("tokio-demo-multi-sensor"),
    );

    // Temperature sensors (outbound to MQTT) - using link_address() from key metadata.
    // TempIndoor needs per-link QoS/retain, so it stays on the raw builder (the
    // escape hatch for per-link options); TempOutdoor/TempServerRoom need no
    // extra options, so `.linked_to()` (from `#[derive(Linkable)]`) suffices.
    builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(indoor_temp_producer)
            .tap(temperature_logger)
            .link_to(SensorKey::TempIndoor.link_address().unwrap())
            .with_qos(1)
            .with_retain(false)
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>(SensorKey::TempOutdoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(outdoor_temp_producer)
            .tap(temperature_logger);
        reg.linked_to(SensorKey::TempOutdoor.link_address().unwrap());
    });

    builder.configure::<Temperature>(SensorKey::TempServerRoom, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(server_room_temp_producer)
            .tap(temperature_logger);
        reg.linked_to(SensorKey::TempServerRoom.link_address().unwrap());
    });

    // Command consumers (inbound from MQTT) - using link_address() from key metadata.
    // TempIndoor needs subscribe QoS, so it stays on the raw builder; TempOutdoor
    // uses `.linked_from()`.
    builder.configure::<TemperatureCommand>(CommandKey::TempIndoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(command_consumer)
            .link_from(CommandKey::TempIndoor.link_address().unwrap())
            .with_qos(1) // subscribe QoS via MqttLinkExt
            .with_deserializer(|_ctx, data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    builder.configure::<TemperatureCommand>(CommandKey::TempOutdoor, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(command_consumer);
        reg.linked_from(CommandKey::TempOutdoor.link_address().unwrap());
    });

    println!("Subscribe: mosquitto_sub -h localhost -t 'sensors/#' -v");
    println!("Command:   mosquitto_pub -h localhost -t 'commands/temp/indoor' -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    println!();
    println!("Press Ctrl+C to stop.\n");

    builder.run().await
}
