//! # Weather Station Alpha
//!
//! Outdoor weather station that fetches real weather data from Open-Meteo API
//! and publishes to the weather mesh via MQTT.
//!
//! Uses Vienna, Austria as the default location.

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey};
use aimdb_data_contracts::Linkable;
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;
use weather_mesh_common::{Humidity, HumidityKey, TempKey, Temperature};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weather_station_alpha=info,aimdb_core=info".into()),
        )
        .init();

    info!("🚀 Starting Weather Station Alpha");

    // Configuration from environment
    let mqtt_broker = std::env::var("MQTT_BROKER").unwrap_or_else(|_| "localhost".to_string());
    let mqtt_url = format!("mqtt://{}", mqtt_broker);

    // Weather station coordinates (Vienna, Austria)
    let lat: f64 = std::env::var("WEATHER_LAT")
        .unwrap_or_else(|_| "48.2082".to_string())
        .parse()?;
    let lon: f64 = std::env::var("WEATHER_LON")
        .unwrap_or_else(|_| "16.3738".to_string())
        .parse()?;

    info!("📡 Connecting to MQTT broker: {}", mqtt_url);
    info!("🌍 Weather location: {:.4}°N, {:.4}°E", lat, lon);

    // Create runtime adapter
    let adapter = Arc::new(TokioAdapter::new()?);

    // Build database with MQTT connector
    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_connector(MqttConnector::new(&mqtt_url).with_client_id("weather-station-alpha"));

    // Configure temperature record
    let temp_topic = TempKey::Alpha.link_address().unwrap();
    builder.configure::<Temperature>(TempKey::Alpha, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(temp_topic)
            .with_serializer(|t: &Temperature| {
                t.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure humidity record
    let humidity_topic = HumidityKey::Alpha.link_address().unwrap();
    builder.configure::<Humidity>(HumidityKey::Alpha, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(humidity_topic)
            .with_serializer(|h: &Humidity| {
                h.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    let db = builder.build().await?;

    info!("✅ Database initialized with 2 record types");
    info!("   - Temperature: {}", temp_topic);
    info!("   - Humidity: {}", humidity_topic);

    // Get producers
    let temp_producer = db.producer::<Temperature>(TempKey::Alpha.as_str());
    let humidity_producer = db.producer::<Humidity>(HumidityKey::Alpha.as_str());

    // Spawn weather data producer
    info!("🌤️  Starting weather data producer...");
    tokio::spawn(async move {
        let client = OpenMeteoClient::new();

        loop {
            match client.fetch_current(lat, lon).await {
                Ok(data) => {
                    let now = chrono::Utc::now().timestamp_millis() as u64;

                    let temp = Temperature::new(data.temperature as f32, now);

                    let humidity = Humidity {
                        percent: data.humidity as f32,
                        timestamp: now,
                    };

                    if let Err(e) = temp_producer.produce(temp).await {
                        tracing::warn!("Failed to produce temperature: {:?}", e);
                    }

                    if let Err(e) = humidity_producer.produce(humidity).await {
                        tracing::warn!("Failed to produce humidity: {:?}", e);
                    }

                    tracing::info!(
                        "📊 Published: {:.1}°C, {:.1}%",
                        data.temperature,
                        data.humidity
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch weather data: {}", e);
                }
            }

            // OpenMeteo updates every 15 minutes, poll every 5 min
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    info!("");
    info!("🎯 Weather Station Alpha ready!");
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

// --- OpenMeteo API Client ---

struct OpenMeteoClient {
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    current: CurrentWeather,
}

#[derive(Debug, Deserialize)]
struct CurrentWeather {
    temperature_2m: f64,
    relative_humidity_2m: f64,
}

struct WeatherData {
    temperature: f64,
    humidity: f64,
}

impl OpenMeteoClient {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    async fn fetch_current(&self, lat: f64, lon: f64) -> Result<WeatherData, reqwest::Error> {
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,relative_humidity_2m",
            lat, lon
        );

        let resp: OpenMeteoResponse = self.client.get(&url).send().await?.json().await?;

        Ok(WeatherData {
            temperature: resp.current.temperature_2m,
            humidity: resp.current.relative_humidity_2m,
        })
    }
}
