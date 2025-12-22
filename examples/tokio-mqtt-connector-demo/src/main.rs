//! MQTT Connector Demo
//!
//! Demonstrates bidirectional MQTT integration with AimDB:
//! - Multiple temperature sensors publishing to different topics
//! - Multiple command consumers receiving from different topics
//!
//! This demo uses `mqtt-connector-demo-common` for shared types and monitors,
//! demonstrating AimDB's "write once, run anywhere" capability.
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

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

// Import shared types and monitors from the common crate
use mqtt_connector_demo_common::{
    command_consumer, temperature_logger, Temperature, TemperatureCommand,
};

// ============================================================================
// TEMPERATURE PRODUCERS (platform-specific)
// ============================================================================

/// Indoor temperature sensor producer
async fn indoor_temp_producer(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🏠 Starting INDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("indoor-001", 22.0 + (i as f32 * 0.5)); // Indoor temps: 22-24°C

        log.info(&format!(
            "🏠 Indoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&format!("❌ Failed to produce indoor temp: {:?}", e));
        }

        time.sleep(time.secs(2)).await;
    }

    log.info("✅ Indoor producer finished");
}

/// Outdoor temperature sensor producer
async fn outdoor_temp_producer(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🌳 Starting OUTDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("outdoor-001", 5.0 + (i as f32 * 1.0)); // Outdoor temps: 5-9°C (cold!)

        log.info(&format!(
            "🌳 Outdoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&format!("❌ Failed to produce outdoor temp: {:?}", e));
        }

        time.sleep(time.secs(2)).await;
    }

    log.info("✅ Outdoor producer finished");
}

/// Server room temperature sensor producer
async fn server_room_temp_producer(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🖥️  Starting SERVER ROOM temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("server-room-001", 18.0 + (i as f32 * 0.2)); // Server room: 18-19°C (cooled)

        log.info(&format!(
            "🖥️  Server room sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&format!("❌ Failed to produce server room temp: {:?}", e));
        }

        time.sleep(time.secs(2)).await;
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

    println!("MQTT Connector Demo");
    println!("===================");
    println!();

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("tokio-demo-multi-sensor"),
    );

    // Temperature sensors (outbound to MQTT)
    // Using shared temperature_logger monitor from common crate
    builder.configure::<Temperature>("sensor.temp.indoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(indoor_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/indoor")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>("sensor.temp.outdoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(outdoor_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/outdoor")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>("sensor.temp.server_room", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(server_room_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/server_room")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // Command consumers (inbound from MQTT)
    // Using shared command_consumer monitor from common crate
    builder.configure::<TemperatureCommand>("command.temp.indoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(command_consumer)
            .link_from("mqtt://commands/temp/indoor")
            .with_deserializer(|data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    builder.configure::<TemperatureCommand>("command.temp.outdoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(command_consumer)
            .link_from("mqtt://commands/temp/outdoor")
            .with_deserializer(|data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    println!("Subscribe: mosquitto_sub -h localhost -t 'sensors/#' -v");
    println!("Command:   mosquitto_pub -h localhost -t 'commands/temp/indoor' -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    println!();
    println!("Press Ctrl+C to stop.\n");

    builder.run().await
}
