//! # Weather Station Beta
//!
//! Indoor weather station that generates synthetic sensor data
//! using the Simulatable trait. Useful for testing and demos
//! without external dependencies.

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey};
use aimdb_data_contracts::Linkable;
#[cfg(feature = "sim")]
use aimdb_data_contracts::{RandomWalkParams, SimProfile, SimulatableRegistrarExt};
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
#[cfg(feature = "sim")]
use rand::SeedableRng;
use std::sync::Arc;
use tracing::info;
use weather_mesh_common::{DewPoint, DewPointKey, Humidity, HumidityKey, TempKey, Temperature};

/// A Send RNG for one simulated record (fresh per source; OS-seeded on std).
#[cfg(feature = "sim")]
fn sim_rng() -> rand::rngs::StdRng {
    rand::rngs::StdRng::from_rng(&mut rand::rng())
}

/// Placeholder "hardware" temperature source for the production (non-sim) build.
///
/// A real deployment reads an I2C/SPI sensor here. The demo emits a fixed
/// reading on the same 5 s cadence so the record still has exactly one writer
/// and the production dependency graph stays sim-free (no `rand`).
#[cfg(not(feature = "sim"))]
async fn read_temperature(
    ctx: aimdb_core::RuntimeContext,
    producer: aimdb_core::Producer<Temperature>,
) {
    loop {
        let now = unix_millis(&ctx);
        producer.produce(Temperature::new(20.0, now));
        ctx.time().sleep_millis(5000).await;
    }
}

/// Placeholder "hardware" humidity source for the production (non-sim) build.
#[cfg(not(feature = "sim"))]
async fn read_humidity(ctx: aimdb_core::RuntimeContext, producer: aimdb_core::Producer<Humidity>) {
    loop {
        let now = unix_millis(&ctx);
        producer.produce(Humidity {
            percent: 45.0,
            timestamp: now,
        });
        ctx.time().sleep_millis(5000).await;
    }
}

#[cfg(not(feature = "sim"))]
fn unix_millis(ctx: &aimdb_core::RuntimeContext) -> u64 {
    ctx.time()
        .unix_time()
        .map(|(s, ns)| s.saturating_mul(1000) + (ns / 1_000_000) as u64)
        .unwrap_or(0)
}

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
    let temp_topic = TempKey::Beta.link_address().unwrap();
    builder.configure::<Temperature>(TempKey::Beta, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 });

        // Sim-to-real is a compile-time `#[cfg]`: exactly one producer installed.
        #[cfg(feature = "sim")]
        reg.simulate(
            SimProfile {
                interval_ms: 5000,
                params: RandomWalkParams {
                    base: 20.0,     // Indoor: ~20°C
                    variation: 3.0, // ±3°C
                    trend: 0.0,
                    step: 0.2,
                },
            },
            sim_rng(),
        );
        #[cfg(not(feature = "sim"))]
        reg.source(read_temperature);

        reg.link_to(temp_topic)
            .with_serializer(|ctx, t: &Temperature| {
                ctx.log().info(&format!(
                    "Serializing temp {:.1}°C @ tick {:?}",
                    t.celsius,
                    ctx.time().now()
                ));
                t.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure humidity record
    let humidity_topic = HumidityKey::Beta.link_address().unwrap();
    builder.configure::<Humidity>(HumidityKey::Beta, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 });

        #[cfg(feature = "sim")]
        reg.simulate(
            SimProfile {
                interval_ms: 5000,
                params: RandomWalkParams {
                    base: 45.0,      // Indoor: ~45%
                    variation: 10.0, // ±10%
                    trend: 0.0,
                    step: 0.2,
                },
            },
            sim_rng(),
        );
        #[cfg(not(feature = "sim"))]
        reg.source(read_humidity);

        reg.link_to(humidity_topic)
            .with_serializer(|ctx, h: &Humidity| {
                ctx.log().info(&format!(
                    "Serializing humidity {:.1}% @ tick {:?}",
                    h.percent,
                    ctx.time().now()
                ));
                h.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure dew point record — derived from temperature and humidity
    let dew_point_topic = DewPointKey::Beta.link_address().unwrap();
    builder.configure::<DewPoint>(DewPointKey::Beta, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .transform_join(|b| {
                b.input::<Temperature>(TempKey::Beta)
                    .input::<Humidity>(HumidityKey::Beta)
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
                                let dew_point = t.celsius - (100.0 - h.percent) / 5.0;
                                let timestamp = t.timestamp.max(h.timestamp);
                                tracing::info!(
                                    "💧 DewPoint computed: {:.2}°C (T={:.1}°C, RH={:.1}%)",
                                    dew_point,
                                    t.celsius,
                                    h.percent
                                );
                                producer.produce(DewPoint {
                                    celsius: dew_point,
                                    timestamp,
                                });
                            }
                        }
                    })
            })
            .link_to(dew_point_topic)
            .with_serializer(|_ctx, d: &DewPoint| {
                d.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    let (_db, runner) = builder.build().await?;
    tokio::spawn(runner.run());

    info!("✅ Database initialized with 3 record types");
    #[cfg(feature = "sim")]
    let mode = "synthetic";
    #[cfg(not(feature = "sim"))]
    let mode = "hardware";
    info!("   - Temperature: {} ({})", temp_topic, mode);
    info!("   - Humidity: {} ({})", humidity_topic, mode);
    info!(
        "   - DewPoint: {} (derived via transform_join)",
        dew_point_topic
    );

    // Producers are installed inside `configure()` (a `.simulate()` source in the
    // `sim` build, a hardware `.source()` otherwise) — no manual spawn needed.

    info!("");
    info!("🎯 Weather Station Beta ready!");
    info!("📡 Publishing to MQTT topics:");
    info!("   - {}", temp_topic);
    info!("   - {}", humidity_topic);
    info!("   - {}", dew_point_topic);
    info!("");
    info!("Press Ctrl+C to stop");

    // Keep running
    tokio::signal::ctrl_c().await?;
    info!("🛑 Shutting down...");

    Ok(())
}
