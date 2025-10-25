//! MQTT Connector Demo - End-to-End Example
//!
//! This example demonstrates the complete MQTT integration flow with
//! **unified runtime abstraction** via the `#[service]` macro.
//!
//! 1. **Create MqttClientPool** - Manages MQTT broker connections
//! 2. **Pass to Builder** - Use `.with_connector_pool()` to enable auto-registration
//! 3. **Configure Records** - Use `.link()` and `.with_serializer()` for each record
//! 4. **Define Services** - Use `#[service]` macro for runtime-agnostic tasks
//! 5. **Automatic Publishing** - Consumers are auto-registered and publish on produce()
//!
//! Features shown:
//! - **Unified `#[service]` macro** - Same code works for Tokio and Embassy
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

use aimdb_core::{service, AimDbBuilder, DbResult, Spawn};
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

/// Temperature producer service using unified #[service] macro
/// This same pattern works for both Tokio and Embassy runtimes!
/// 
/// Note: Uses Arc<AimDb> for Tokio (shared ownership across async tasks)
///       Uses &'static AimDb for Embassy (static storage requirement)
#[service]
async fn temperature_producer(db: Arc<aimdb_core::AimDb>) {
    println!("📊 Starting temperature producer service...\n");
    
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
        if let Err(e) = db.produce(temp.clone()).await {
            eprintln!("❌ Failed to produce temperature: {:?}", e);
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("\n✅ Published 10 temperature readings");
}

#[tokio::main]
async fn main() -> DbResult<()> {
    // Initialize tracing
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    // Create runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);

    // Create MQTT client pool connected to a specific broker
    // Each pool manages ONE broker connection
    let mqtt_pool = Arc::new(
        aimdb_mqtt_connector::MqttClientPool::new("mqtt://localhost:1883")
            .await
            .expect("Failed to create MQTT pool"),
    );

    println!("🔧 Creating database with MQTT connector pool...");

    // Build database with MQTT connector using scheme-based registration
    let mut builder = AimDbBuilder::new()
        .with_runtime(runtime.clone())
        .with_connector_pool("mqtt", mqtt_pool.clone()); // Register the pool for "mqtt://" scheme

    builder.configure::<Temperature>(|reg| {
        // Register producer (application logic) and chain the MQTT connectors
        reg.producer(|_em, temp| async move {
            println!(
                "Temperature produced: {:.1}°C from {}",
                temp.celsius, temp.sensor_id
            );
        })
        // Register MQTT connector with serializer
        // The .link().with_serializer().finish() pattern automatically:
        // 1. Creates a consumer that subscribes to Temperature updates
        // 2. Serializes each value using the provided callback
        // 3. Publishes to the MQTT broker via the connector pool
        //
        // URL format: mqtt://topic (broker info is already in the pool)
        .link("mqtt://sensors/temperature")
        .with_serializer(|temp: &Temperature| {
            // Custom JSON serialization
            serde_json::to_vec(temp).map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
        })
        .finish()
        // You can add multiple MQTT destinations
        // Each gets its own consumer with independent serialization
        .link("mqtt://sensors/raw")
        .with_config("qos", "1") // Quality of Service level 1
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
    println!("   - mqtt://sensors/temperature (JSON format)");
    println!("   - mqtt://sensors/raw (CSV format, QoS=1, retain=true)");
    println!("   Broker: localhost:1883\n");

    // Wrap database in Arc for shared ownership across async tasks
    // This is the idiomatic Tokio pattern - no statics needed!
    let db_arc = Arc::new(db);

    // Spawn temperature producer service
    // Arc allows the service to outlive the current scope
    runtime.spawn(temperature_producer(db_arc)).unwrap();

    // Keep running to allow MQTT publishing to complete
    tokio::time::sleep(Duration::from_secs(12)).await;

    Ok(())
}
