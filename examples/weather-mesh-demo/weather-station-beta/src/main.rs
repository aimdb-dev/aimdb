//! # Weather Station Beta
//!
//! Indoor weather station that generates synthetic sensor data
//! using the Simulatable trait. Useful for testing and demos
//! without external dependencies.

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey};
use aimdb_data_contracts::{Linkable, Simulatable, SimulationConfig, SimulationParams};
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use rand::SeedableRng;
use std::sync::Arc;
use tracing::info;
use weather_mesh_common::{Humidity, NodeKey, Temperature};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weather_station_beta=info,aimdb_core=info".into()),
        )
        .init();

    info!("🚀 Starting Weather Station Beta (Synthetic)");

    // Configuration from environment
    let mqtt_broker = std::env::var("MQTT_BROKER").unwrap_or_else(|_| "localhost".to_string());
    let mqtt_url = format!("mqtt://{}", mqtt_broker);

    info!("📡 Connecting to MQTT broker: {}", mqtt_url);

    // Create runtime adapter
    let adapter = Arc::new(TokioAdapter::new()?);

    // Build database with MQTT connector
    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_connector(MqttConnector::new(&mqtt_url).with_client_id("weather-station-beta"));

    // Configure temperature record
    let temp_topic = format!("{}temperature", NodeKey::Beta.link_address().unwrap());
    builder.configure::<Temperature>(NodeKey::Beta, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(&temp_topic)
            .with_serializer(|t: &Temperature| {
                t.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure humidity record
    let humidity_topic = format!("{}humidity", NodeKey::Beta.link_address().unwrap());
    builder.configure::<Humidity>(NodeKey::Beta, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(&humidity_topic)
            .with_serializer(|h: &Humidity| {
                h.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    let db = builder.build().await?;

    info!("✅ Database initialized with 2 record types");
    info!("   - Temperature: {} (synthetic)", temp_topic);
    info!("   - Humidity: {} (synthetic)", humidity_topic);

    // Get producers
    let temp_producer = db.producer::<Temperature>(NodeKey::Beta.as_str());
    let humidity_producer = db.producer::<Humidity>(NodeKey::Beta.as_str());

    // Spawn synthetic data producer
    info!("🎲 Starting synthetic data producer...");
    tokio::spawn(async move {
        // Simulation configuration for indoor conditions
        let temp_config = SimulationConfig {
            enabled: true,
            interval_ms: 5000,
            params: SimulationParams {
                base: 20.0,     // Indoor: ~20°C
                variation: 3.0, // ±3°C
                step: 0.2,      // Smooth random walk
                trend: 0.0,     // No trend
            },
        };

        let humidity_config = SimulationConfig {
            enabled: true,
            interval_ms: 5000,
            params: SimulationParams {
                base: 45.0,      // Indoor: ~45%
                variation: 10.0, // ±10%
                step: 0.2,       // Smooth random walk
                trend: 0.0,      // No trend
            },
        };

        // Create RNG (StdRng is Send, ThreadRng is not)
        let mut rng = rand::rngs::StdRng::from_entropy();
        let mut prev_temp: Option<Temperature> = None;
        let mut prev_humidity: Option<Humidity> = None;

        loop {
            let now = chrono::Utc::now().timestamp_millis() as u64;

            // Generate synthetic readings using Simulatable trait
            let temp = Temperature::simulate(&temp_config, prev_temp.as_ref(), &mut rng, now);
            let humidity =
                Humidity::simulate(&humidity_config, prev_humidity.as_ref(), &mut rng, now);

            // Produce the readings
            if let Err(e) = temp_producer.produce(temp.clone()).await {
                tracing::warn!("Failed to produce temperature: {:?}", e);
            }

            if let Err(e) = humidity_producer.produce(humidity.clone()).await {
                tracing::warn!("Failed to produce humidity: {:?}", e);
            }

            tracing::info!(
                "📊 Published: {:.1}°C, {:.1}%",
                temp.celsius,
                humidity.percent
            );

            // Store for next iteration (random walk)
            prev_temp = Some(temp);
            prev_humidity = Some(humidity);

            // Update every 5 seconds for smooth demo
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });

    info!("");
    info!("🎯 Weather Station Beta ready!");
    info!("📡 Publishing to MQTT topics:");
    info!("   - {}", temp_topic);
    info!("   - {}", humidity_topic);
    info!("");
    info!("Press Ctrl+C to stop");

    // Keep running
    tokio::signal::ctrl_c().await?;
    info!("🛑 Shutting down...");

    Ok(())
}
