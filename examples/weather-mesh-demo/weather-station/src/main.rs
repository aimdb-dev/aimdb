//! # Weather Station (generic)
//!
//! The public-mesh station binary (design 042 §7/§8): configured entirely by
//! the `station.toml` profile that `aimdb join` writes — broker URL and
//! credentials, assigned slot, station name, and the (coarsened) location the
//! real weather data is fetched for. One profile, one command:
//!
//! ```bash
//! aimdb join https://mesh.aimdb.dev            # writes station.toml
//! cargo run -p weather-station -- --config station.toml
//! ```
//!
//! Same contracts, same Open-Meteo source as station alpha — only the key
//! namespace differs: this station publishes into its assigned slot
//! (`station/{slot}/…`) instead of a compile-time enum topic.

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey, StringKey};
use aimdb_data_contracts::Linkable;
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use clap::Parser;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;
use weather_mesh_common::{DewPoint, Humidity, Temperature};

/// Generic cloud weather station — runs from a `station.toml` profile
#[derive(Debug, Parser)]
#[command(name = "weather-station", version, about)]
struct Cli {
    /// Path to the station profile written by `aimdb join` (design 043 §4)
    #[arg(long)]
    config: std::path::PathBuf,
}

/// The station profile (design 043 §4). Unknown fields are ignored so the
/// service can extend the format without a version bump.
#[derive(Debug, Deserialize)]
struct StationProfile {
    profile_version: u64,
    station_id: String,
    broker: BrokerProfile,
    app: AppProfile,
}

#[derive(Debug, Deserialize)]
struct BrokerProfile {
    url: String,
    username: String,
    password: String,
}

/// The flagship's `app` map: name + service-coarsened coordinates (042 D8).
#[derive(Debug, Deserialize)]
struct AppProfile {
    name: String,
    lat: f64,
    lon: f64,
}

#[tokio::main]
async fn main() {
    // Display-format errors (revoked slot, bad profile, …) instead of the
    // default Debug dump — these messages are the failure UX (design 042 §9).
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weather_station=info,aimdb_core=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let profile: StationProfile = toml::from_str(&std::fs::read_to_string(&cli.config)?)?;

    if profile.profile_version != 1 {
        return Err(format!(
            "unsupported profile_version {} (this station understands 1) — \
             re-run `aimdb join` with a current CLI",
            profile.profile_version
        )
        .into());
    }

    // The flagship assigns slot-scoped identities: station_id = "slot-<n>"
    // maps onto the hub's pool records station.<n>.* (design 042 §4).
    let slot: u16 = profile
        .station_id
        .strip_prefix("slot-")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            format!(
                "station_id '{}' is not of the form slot-<n> — \
                 is this profile from a weather-mesh deployment?",
                profile.station_id
            )
        })?;

    info!("🚀 Starting Weather Station \"{}\"", profile.app.name);
    info!(
        "📡 Broker: {} (slot {})",
        redact_url(&profile.broker.url),
        slot
    );
    info!(
        "🌍 Weather location: {:.2}°N, {:.2}°E",
        profile.app.lat, profile.app.lon
    );

    let client_id = format!("weather-station-{slot}");

    // Fail fast on a dead credential instead of retrying forever: a revoked
    // slot should say so at the moment the user is looking (design 042 §9).
    preflight_broker_check(&profile.broker, &client_id).await?;

    let mqtt_url = url_with_credentials(&profile.broker)?;

    // Create runtime adapter
    let adapter = Arc::new(TokioAdapter::new()?);

    // Build database with MQTT connector
    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_connector(MqttConnector::new(&mqtt_url).with_client_id(&client_id));

    // Configure temperature record
    let temp_key = StringKey::intern(format!("station.{slot}.temperature"));
    let temp_topic = format!("mqtt://station/{slot}/temperature");
    builder.configure::<Temperature>(temp_key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(&temp_topic)
            .with_serializer(|_ctx, t: &Temperature| {
                t.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure humidity record
    let humidity_key = StringKey::intern(format!("station.{slot}.humidity"));
    let humidity_topic = format!("mqtt://station/{slot}/humidity");
    builder.configure::<Humidity>(humidity_key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .link_to(&humidity_topic)
            .with_serializer(|_ctx, h: &Humidity| {
                h.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    // Configure dew point record — derived from temperature and humidity
    let dew_point_key = StringKey::intern(format!("station.{slot}.dew_point"));
    let dew_point_topic = format!("mqtt://station/{slot}/dew_point");
    builder.configure::<DewPoint>(dew_point_key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .transform_join(|b| {
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
            })
            .link_to(&dew_point_topic)
            .with_serializer(|_ctx, d: &DewPoint| {
                d.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish();
    });

    let (db, runner) = builder.build().await?;
    tokio::spawn(runner.run());

    info!("✅ Database initialized with 3 record types");

    // Get producers
    let temp_producer = db.producer::<Temperature>(temp_key.as_str())?;
    let humidity_producer = db.producer::<Humidity>(humidity_key.as_str())?;

    // Spawn weather data producer
    info!("🌤️  Starting weather data producer...");
    let station_name = profile.app.name.clone();
    let (lat, lon) = (profile.app.lat, profile.app.lon);
    tokio::spawn(async move {
        let client = OpenMeteoClient::new();
        let mut first_publish = true;

        loop {
            match client.fetch_current(lat, lon).await {
                Ok(data) => {
                    let now = chrono::Utc::now().timestamp_millis() as u64;

                    temp_producer.produce(Temperature::new(data.temperature as f32, now));
                    humidity_producer.produce(Humidity {
                        percent: data.humidity as f32,
                        timestamp: now,
                    });

                    tracing::info!(
                        "📊 Published: {:.1}°C, {:.1}%",
                        data.temperature,
                        data.humidity
                    );

                    if first_publish {
                        first_publish = false;
                        print_live_banner(&station_name);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch weather data: {}", e);
                }
            }

            // OpenMeteo updates every 15 minutes, poll every 5 min
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });

    info!("");
    info!("🎯 Weather Station \"{}\" ready!", profile.app.name);
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

/// The shareable moment (design 042 §8, tier 2): after the first successful
/// publish, hand the user their station's live chart and a ready-made MCP
/// prompt for its own data.
fn print_live_banner(name: &str) {
    println!();
    println!("🗺️  Your station is live: https://aimdb.dev/mesh/{name}");
    println!("🤖 Ask any MCP agent about it — point it at https://aimdb.dev/mcp and try:");
    println!("      \"What is the latest temperature and dew point at station {name}?\"");
    println!();
}

/// One CONNECT → CONNACK round-trip before building the database.
///
/// The connector's event loop retries connection errors forever (by design —
/// brokers come and go). For an admission-scoped credential that is the wrong
/// UX: a revoked slot would retry silently for eternity. Probe once and turn
/// an auth rejection into a human answer (design 042 §9).
async fn preflight_broker_check(
    broker: &BrokerProfile,
    client_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (tls, host, port) = split_broker_url(&broker.url)?;

    let mut opts = rumqttc::MqttOptions::new(format!("{client_id}-preflight"), host, port);
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_credentials(&broker.username, &broker.password);
    if tls {
        opts.set_transport(rumqttc::Transport::Tls(rumqttc::TlsConfiguration::Native));
    }

    let (client, mut event_loop) = rumqttc::AsyncClient::new(opts, 4);
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match event_loop.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_))) => return Ok(()),
                Ok(_) => continue,
                Err(e) => return Err(e),
            }
        }
    })
    .await;
    let _ = client.disconnect().await;

    match result {
        Ok(Ok(())) => {
            info!("✅ Broker accepted the station credential");
            Ok(())
        }
        Ok(Err(rumqttc::ConnectionError::ConnectionRefused(code))) => Err(format!(
            "the broker rejected this station's credential ({code:?}).\n  \
             The slot was likely revoked (silent for 30 days, or by the operator).\n  \
             Re-join the mesh for a fresh slot: aimdb join <provisioning-url>"
        )
        .into()),
        Ok(Err(e)) => Err(format!(
            "cannot reach the broker at {}: {e}",
            redact_url(&broker.url)
        )
        .into()),
        Err(_) => Err(format!(
            "timed out connecting to the broker at {}",
            redact_url(&broker.url)
        )
        .into()),
    }
}

/// Split a `mqtt[s]://host[:port]` broker URL into (tls, host, port).
fn split_broker_url(url: &str) -> Result<(bool, String, u16), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("broker URL '{url}' has no scheme"))?;
    let tls = match scheme {
        "mqtt" => false,
        "mqtts" => true,
        other => return Err(format!("unsupported broker scheme '{other}'")),
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse()
                .map_err(|_| format!("invalid broker port in '{url}'"))?,
        ),
        None => (authority.to_string(), if tls { 8883 } else { 1883 }),
    };
    Ok((tls, host, port))
}

/// Embed the profile's credential into the connector URL
/// (`mqtts://user:pass@host:port`), the form the MQTT connector parses.
fn url_with_credentials(broker: &BrokerProfile) -> Result<String, String> {
    let (scheme, rest) = broker
        .url
        .split_once("://")
        .ok_or_else(|| format!("broker URL '{}' has no scheme", broker.url))?;
    Ok(format!(
        "{scheme}://{}:{}@{rest}",
        broker.username, broker.password
    ))
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

// --- OpenMeteo API Client (same source as station alpha) ---

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

#[cfg(test)]
mod tests {
    use super::*;

    fn broker(url: &str) -> BrokerProfile {
        BrokerProfile {
            url: url.to_string(),
            username: "station-17".to_string(),
            password: "s3cret".to_string(),
        }
    }

    #[test]
    fn profile_parses_the_043_sample() {
        let profile: StationProfile = toml::from_str(
            r#"
            profile_version = 1
            station_id = "slot-17"

            [broker]
            url = "mqtts://xxxx.eu-central-1.emqx.cloud:8883"
            username = "station-17"
            password = "s3cret"

            [app]
            name = "graz-balcony"
            lat = 47.07
            lon = 15.44
            "#,
        )
        .unwrap();
        assert_eq!(profile.profile_version, 1);
        assert_eq!(profile.station_id, "slot-17");
        assert_eq!(profile.app.name, "graz-balcony");
    }

    #[test]
    fn broker_url_splits_with_scheme_defaults() {
        assert_eq!(
            split_broker_url("mqtts://broker.example.com:8883").unwrap(),
            (true, "broker.example.com".to_string(), 8883)
        );
        assert_eq!(
            split_broker_url("mqtts://broker.example.com").unwrap(),
            (true, "broker.example.com".to_string(), 8883)
        );
        assert_eq!(
            split_broker_url("mqtt://localhost").unwrap(),
            (false, "localhost".to_string(), 1883)
        );
        assert!(split_broker_url("http://x").is_err());
        assert!(split_broker_url("no-scheme").is_err());
    }

    #[test]
    fn connector_url_carries_credentials() {
        assert_eq!(
            url_with_credentials(&broker("mqtts://broker.example.com:8883")).unwrap(),
            "mqtts://station-17:s3cret@broker.example.com:8883"
        );
    }

    #[test]
    fn redaction_hides_credentials() {
        assert_eq!(
            redact_url("mqtts://station-17:s3cret@broker.example.com:8883"),
            "mqtts://…@broker.example.com:8883"
        );
        assert_eq!(redact_url("mqtt://localhost"), "mqtt://localhost");
    }
}
