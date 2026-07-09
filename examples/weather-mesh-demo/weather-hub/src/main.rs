use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey, StringKey};
use aimdb_data_contracts::{Linkable, ObservableRegistrarExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use weather_mesh_common::{DewPoint, Humidity, HumidityKey, TempKey, Temperature};

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

    // Broker: MQTT_URL takes a full connector URL (e.g. the public mesh broker,
    // mqtts://hub-sub:…@xxxx.eu-central-1.emqx.cloud:8883); otherwise fall back
    // to the local demo's host-only MQTT_BROKER variable.
    let mqtt_url = std::env::var("MQTT_URL").unwrap_or_else(|_| {
        let mqtt_broker = std::env::var("MQTT_BROKER").unwrap_or_else(|_| "localhost".to_string());
        format!("mqtt://{}", mqtt_broker)
    });

    tracing::info!("📡 Connecting to MQTT broker: {}", redact_url(&mqtt_url));

    let runtime = std::sync::Arc::new(TokioAdapter::new()?);

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new(&mqtt_url).with_client_id("weather-hub"),
    );

    // MESH_SLOTS switches the hub into public-mesh mode (design 042 §4): a
    // bounded pool of string-keyed slot records instead of the closed
    // three-station enums. Unset = the local docker-compose demo, unchanged.
    match std::env::var("MESH_SLOTS").ok() {
        Some(v) => {
            let slots: u16 = v.parse().unwrap_or(64);
            tracing::info!("🕸️  Mesh mode: {} slots", slots);
            configure_mesh(&mut builder, slots);
        }
        None => configure_demo(&mut builder),
    }

    tracing::info!("✅ Weather Hub configured, starting...");

    builder.run().await
}

/// The local demo configuration: the three closed-enum stations.
fn configure_demo(builder: &mut AimDbBuilder) {
    // Configure Temperature records for all nodes with tap functions
    for key in [TempKey::Alpha, TempKey::Beta, TempKey::Gamma] {
        let topic = key.link_address().unwrap().to_string();

        builder.configure::<Temperature>(key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
            // Metrics: fold °C into the record's signal gauge (record.list/get).
            reg.observe();
            // Console: one human-readable log line per value (the demo's point).
            reg.log(key.as_str());
            reg.link_from(&topic)
                .with_deserializer(|ctx, data: &[u8]| {
                    ctx.log()
                        .debug("Deserializing temperature from MQTT payload");
                    let temp = Temperature::from_bytes(data)?;
                    ctx.log().info(&format!(
                        "🌡️  Received: {:.1}°C (deserialized at runtime tick {:?})",
                        temp.celsius,
                        ctx.time().now()
                    ));
                    Ok(temp)
                })
                .finish();
        });
    }

    // Configure Humidity records for all nodes with tap functions
    for key in [HumidityKey::Alpha, HumidityKey::Beta, HumidityKey::Gamma] {
        let topic = key.link_address().unwrap().to_string();

        builder.configure::<Humidity>(key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
            // Metrics only for humidity; temperature keeps the console `.log()`.
            reg.observe();
            reg.link_from(&topic)
                .with_deserializer(|ctx, data: &[u8]| {
                    ctx.log().debug("Deserializing humidity from MQTT payload");
                    let humidity = Humidity::from_bytes(data)?;
                    ctx.log().info(&format!(
                        "💧 Received: {:.1}% humidity (deserialized at runtime tick {:?})",
                        humidity.percent,
                        ctx.time().now()
                    ));
                    Ok(humidity)
                })
                .finish();
        });
    }
}

/// The public-mesh slot pool (design 042 §4).
///
/// Every slot's records exist from startup; admission just assigns an unused
/// slot number, so joining never needs a hub recompile. A malformed payload is
/// rejected by `from_bytes` (contract deserialization) — that error surfacing
/// at the hub is the admission-time schema check.
///
/// No per-value `.log()` here: 64 slots of per-message console lines is noise.
/// `.observe()` keeps the signal gauges on `record list`/`record get`.
fn configure_mesh(builder: &mut AimDbBuilder, slots: u16) {
    for slot in 0..slots {
        let temp_key = StringKey::intern(format!("station.{slot}.temperature"));
        let temp_topic = format!("mqtt://station/{slot}/temperature");
        builder.configure::<Temperature>(temp_key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
            reg.observe();
            reg.link_from(&temp_topic)
                .with_deserializer(|_ctx, data: &[u8]| Temperature::from_bytes(data))
                .finish();
        });

        let humidity_key = StringKey::intern(format!("station.{slot}.humidity"));
        let humidity_topic = format!("mqtt://station/{slot}/humidity");
        builder.configure::<Humidity>(humidity_key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
            reg.observe();
            reg.link_from(&humidity_topic)
                .with_deserializer(|_ctx, data: &[u8]| Humidity::from_bytes(data))
                .finish();
        });

        // Derived at the hub so the dashboard stays supplied even when a
        // station publishes only temperature/humidity.
        let dew_point_key = StringKey::intern(format!("station.{slot}.dew_point"));
        builder.configure::<DewPoint>(dew_point_key, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
            reg.observe();
            reg.transform_join(|b| {
                b.input::<Temperature>(temp_key)
                    .input::<Humidity>(humidity_key)
                    .on_triggers(|mut rx, producer| async move {
                        let mut last_temp: Option<Temperature> = None;
                        let mut last_hum: Option<Humidity> = None;
                        while let Ok(trigger) = rx.recv().await {
                            match trigger.index() {
                                0 => last_temp = trigger.as_input::<Temperature>().cloned(),
                                1 => last_hum = trigger.as_input::<Humidity>().cloned(),
                                _ => {}
                            }
                            if let (Some(t), Some(h)) = (&last_temp, &last_hum) {
                                // Magnus approximation: T_dp ≈ T - (100 - RH) / 5
                                let dew_point = t.celsius - (100.0 - h.percent) / 5.0;
                                let timestamp = t.timestamp.max(h.timestamp);
                                producer.produce(DewPoint {
                                    celsius: dew_point,
                                    timestamp,
                                });
                            }
                        }
                    })
            });
        });
    }
}

/// Strip URL-embedded credentials before logging.
fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_creds, host)) = rest.rsplit_once('@') {
            return format!("{scheme}://…@{host}");
        }
    }
    url.to_string()
}
