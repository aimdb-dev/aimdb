//! MQTT Connector Demo - End-to-End Example
//!
//! This example demonstrates the complete MQTT integration flow:
//!
//! 1. **Create MqttClientPool** - Manages MQTT broker connections
//! 2. **Pass to Builder** - Use `.with_connector_pool()` to enable auto-registration
//! 3. **Configure Records** - Use `.link()` and `.with_serializer()` for each record
//! 4. **Automatic Publishing** - Consumers are auto-registered and publish on produce()
//!
//! Features shown:
//! - Multiple MQTT destinations with different serialization formats
//! - Filtering (conditional publishing based on value)
//! - Optional connection validation
//! - Both plain MQTT and secure MQTTS connections
//!
//! ## Running the Demo
//!
//! You'll need an MQTT broker running. Quick start with Mosquitto:
//!
//! ```bash
//! docker run -d -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
//! ```
//!
//! Then in another terminal, subscribe to see the messages:
//!
//! ```bash
//! mosquitto_sub -h localhost -t 'sensors/#' -v
//! ```
//!
//! Run the demo:
//!
//! ```bash
//! cargo run --example mqtt-connector-demo --features tokio-runtime,tracing
//! ```

use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_tokio_adapter::TokioAdapter;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Temperature sensor reading
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Temperature {
    sensor_id: String,
    celsius: f32,
    timestamp: u64,
}

#[tokio::main]
async fn main() -> DbResult<()> {
    // Initialize tracing
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    // Create runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);

    // Create MQTT client pool
    // This manages MQTT connections and is shared across all MQTT connectors
    let mqtt_pool = Arc::new(aimdb_mqtt_connector::MqttClientPool::new());

    println!("🔧 Creating database with MQTT connector pool...");

    // Build database with MQTT connector
    let mut builder = AimDbBuilder::new()
        .with_runtime(runtime.clone())
        .with_connector_pool(mqtt_pool.clone());  // Pass the pool to enable auto-registration
    
    builder.configure::<Temperature>(|reg| {
        // Register producer (application logic) and chain the MQTT connectors
        reg.producer(|_em, temp| async move {
                println!("Temperature produced: {:.1}°C from {}", temp.celsius, temp.sensor_id);
            })
            // Register MQTT connector with serializer
            // The .link().with_serializer().finish() pattern automatically:
            // 1. Creates a consumer that subscribes to Temperature updates
            // 2. Serializes each value using the provided callback
            // 3. Publishes to the MQTT broker via the connector pool
            .link("mqtt://localhost:1883/sensors/temperature")
                .with_serializer(|temp: &Temperature| {
                    // Custom JSON serialization
                    serde_json::to_vec(temp).map_err(|e| e.to_string())
                })
                .finish()
            // You can add multiple MQTT destinations
            // Each gets its own consumer with independent serialization
            .link("mqtt://localhost:1883/sensors/raw")
                .with_config("qos", "1")      // Quality of Service level 1
                .with_config("retain", "true") // Retain last message on broker
                .with_serializer(|temp: &Temperature| {
                    // Example: Send as simple CSV format
                    let csv = format!("{},{},{}", temp.sensor_id, temp.celsius, temp.timestamp);
                    Ok(csv.into_bytes())
                })
                .finish();
    });
    
    let db = builder.build()?;

    println!("✅ Database built with MQTT connectors");
    println!("   - mqtt://localhost:1883/sensors/temperature (JSON format)");
    println!("   - mqtt://localhost:1883/sensors/raw (CSV format, QoS=1, retain=true)");
    println!("   Note: Connections will be established automatically on first publish\n");

    // Simulate sensor readings
    println!("📊 Starting to produce temperature readings...\n");
    
    for i in 0..10 {
        let temp = Temperature {
            sensor_id: "sensor-001".to_string(),
            celsius: 20.0 + (i as f32 * 2.0), // 20°C to 38°C
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        // Produce temperature reading
        // This will:
        // 1. Call the producer (console log)
        // 2. Trigger MQTT consumers (serialize and publish to both topics)
        db.produce(temp).await?;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("✅ Published 10 temperature readings");
    
    // Keep running to allow MQTT publishing to complete
    tokio::time::sleep(Duration::from_secs(2)).await;

    Ok(())
}
