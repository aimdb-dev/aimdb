//! MQTT Connector Demo - Bidirectional
//!
//! Demonstrates bidirectional MQTT integration:
//! - Inbound: Subscribe to MQTT commands → process in AimDB
//! - Outbound: Publish temperature readings → MQTT topics
//!
//! ## Running
//!
//! Start an MQTT broker:
//! ```bash
//! docker run -d -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
//! ```
//!
//! In one terminal, subscribe to temperature readings:
//! ```bash
//! mosquitto_sub -h localhost -t 'sensors/#' -v
//! ```
//!
//! In another terminal, publish commands:
//! ```bash
//! mosquitto_pub -h localhost -t 'commands/temperature' -m '{"action":"read","sensor_id":"sensor-001"}'
//! ```
//!
//! Run the demo:
//! ```bash
//! cargo run --example mqtt-connector-demo --features tokio-runtime,tracing
//! ```

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temperature {
    sensor_id: String,
    celsius: f32,
    timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TemperatureCommand {
    action: String,
    sensor_id: String,
}

/// Simulates a temperature sensor generating readings every second
async fn temperature_producer(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("📊 Starting temperature producer service...\n");

    for i in 0..10 {
        let temp = Temperature {
            sensor_id: "sensor-001".to_string(),
            celsius: 20.0 + (i as f32 * 2.0),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        if let Err(e) = temperature.produce(temp).await {
            log.error(&format!("❌ Failed to produce temperature: {:?}", e));
        }

        time.sleep(time.secs(1)).await;
    }

    log.info("\n✅ Published 10 temperature readings");
}

/// Consumer that logs temperature readings
async fn temperature_consumer(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<Temperature, TokioAdapter>,
) {
    let log = ctx.log();

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to temperature buffer");
        return;
    };

    while let Ok(temp) = reader.recv().await {
        log.info(&format!(
            "Temperature produced: {:.1}°C from {}",
            temp.celsius, temp.sensor_id
        ));
    }
}

/// Consumer that processes incoming commands from MQTT
async fn command_consumer(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<TemperatureCommand, TokioAdapter>,
) {
    let log = ctx.log();

    log.info("📥 Command consumer started - waiting for MQTT commands...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to command buffer");
        return;
    };

    while let Ok(cmd) = reader.recv().await {
        log.info(&format!(
            "📨 Received command from MQTT: action='{}' sensor_id='{}'",
            cmd.action, cmd.sensor_id
        ));

        // Process command (in real app, this would trigger sensor reading)
        match cmd.action.as_str() {
            "read" => log.info(&format!("  → Would read from sensor {}", cmd.sensor_id)),
            "reset" => log.info(&format!("  → Would reset sensor {}", cmd.sensor_id)),
            _ => log.warn(&format!("  ⚠️  Unknown action: {}", cmd.action)),
        }
    }
}

#[tokio::main]
async fn main() -> DbResult<()> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(TokioAdapter::new()?);

    println!("🔧 Creating database with bidirectional MQTT connector...");

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("tokio-demo-001"),
    );

    // Configure Temperature record (outbound: AimDB → MQTT)
    builder.configure::<Temperature>("sensor.temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(temperature_producer)
            .tap(temperature_consumer)
            // Publish to MQTT as JSON
            .link_to("mqtt://sensors/temperature")
            .with_serializer(|temp: &Temperature| {
                serde_json::to_vec(temp)
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish()
            // Publish to MQTT as CSV with QoS 1 and retain
            .link_to("mqtt://sensors/raw")
            .with_config("qos", "1")
            .with_config("retain", "true")
            .with_serializer(|temp: &Temperature| {
                let csv = format!("{},{},{}", temp.sensor_id, temp.celsius, temp.timestamp);
                Ok(csv.into_bytes())
            })
            .finish();
    });

    // Configure TemperatureCommand record (inbound: MQTT → AimDB)
    builder.configure::<TemperatureCommand>("command.temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .tap(command_consumer)
            // Subscribe from MQTT commands topic
            .link_from("mqtt://commands/temperature")
            .with_deserializer(|data: &[u8]| {
                serde_json::from_slice::<TemperatureCommand>(data)
                    .map_err(|e| format!("Failed to deserialize command: {}", e))
            })
            .finish();
    });

    println!("✅ Database configured with bidirectional MQTT:");
    println!("   OUTBOUND (AimDB → MQTT):");
    println!("     - mqtt://sensors/temperature (JSON format)");
    println!("     - mqtt://sensors/raw (CSV format, QoS=1, retain=true)");
    println!("   INBOUND (MQTT → AimDB):");
    println!("     - mqtt://commands/temperature (JSON commands)");
    println!("   Broker: localhost:1883");
    println!("\n💡 Try publishing a command:");
    println!("   mosquitto_pub -h localhost -t 'commands/temperature' -m '{{\"action\":\"read\",\"sensor_id\":\"sensor-001\"}}'");
    println!("\n   Press Ctrl+C to stop.\n");

    builder.run().await
}
