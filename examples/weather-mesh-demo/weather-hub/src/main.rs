use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey};
use aimdb_data_contracts::{log_tap, Linkable};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use weather_mesh_common::{Humidity, HumidityKey, TempKey, Temperature};

#[tokio::main]
async fn main() -> aimdb_core::DbResult<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weather_hub=info,aimdb_core=info".into()),
        )
        .init();

    tracing::info!("🚀 Starting Weather Hub");

    // Configuration from environment
    let mqtt_broker = std::env::var("MQTT_BROKER").unwrap_or_else(|_| "localhost".to_string());
    let mqtt_url = format!("mqtt://{}", mqtt_broker);

    tracing::info!("📡 Connecting to MQTT broker: {}", mqtt_url);

    let runtime = std::sync::Arc::new(TokioAdapter::new()?);

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new(&mqtt_url).with_client_id("weather-hub"),
    );

    // Configure Temperature records for all nodes with tap functions
    for key in [TempKey::Alpha, TempKey::Beta, TempKey::Gamma] {
        let topic = key.link_address().unwrap().to_string();

        builder.configure::<Temperature>(key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
                .tap(move |ctx, consumer| log_tap(ctx, consumer, key.as_str()))
                .link_from(&topic)
                .with_deserializer(Temperature::from_bytes)
                .finish();
        });
    }

    // Configure Humidity records for all nodes with tap functions
    for key in [HumidityKey::Alpha, HumidityKey::Beta, HumidityKey::Gamma] {
        let topic = key.link_address().unwrap().to_string();

        builder.configure::<Humidity>(key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
                .tap(move |ctx, consumer| log_tap(ctx, consumer, key.as_str()))
                .link_from(&topic)
                .with_deserializer(Humidity::from_bytes)
                .finish();
        });
    }

    tracing::info!("✅ Weather Hub configured, starting...");

    builder.run().await
}
