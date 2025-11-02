//! Remote Access Demo - Server
//!
//! Demonstrates AimX v1 remote access protocol with record.list functionality.
//!
//! This server:
//! - Creates a database with several example records
//! - Enables remote access on a Unix domain socket
//! - Allows clients to list and inspect registered records
//!
//! Run with:
//! ```
//! cargo run --bin server
//! ```

use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

/// Temperature sensor reading
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temperature {
    sensor_id: String,
    celsius: f64,
    timestamp: u64,
}

/// System status information
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SystemStatus {
    uptime_seconds: u64,
    cpu_usage: f64,
    memory_usage: f64,
}

/// User event log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserEvent {
    user_id: u64,
    event_type: String,
    message: String,
}

/// Configuration settings (has producer, NOT remotely writable)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    app_name: String,
    version: String,
    debug_mode: bool,
}

/// Application settings (NO producer, remotely writable)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppSettings {
    log_level: String,
    max_connections: u32,
    feature_flag_alpha: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("info,aimdb_core=debug")
        .init();

    info!("🚀 Starting AimDB Remote Access Demo Server");

    // Create runtime adapter
    let adapter = Arc::new(TokioAdapter);

    // Configure remote access
    let socket_path = "/tmp/aimdb-demo.sock";

    // Remove existing socket if present
    let _ = std::fs::remove_file(socket_path);

    let mut security_policy = SecurityPolicy::read_write();
    security_policy.allow_write::<AppSettings>();

    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(security_policy)
        .max_connections(10)
        .subscription_queue_size(100);

    info!("📡 Remote access will be available at: {}", socket_path);
    info!("🔒 Security policy: ReadWrite");
    info!("✍️  Writable records: AppSettings");

    // Build database with remote access enabled
    let mut builder = AimDbBuilder::new()
        .runtime(adapter)
        .with_remote_access(remote_config);

    // Configure records
    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SingleLatest).with_serialization();
    });

    builder.configure::<SystemStatus>(|reg| {
        reg.buffer(BufferCfg::SingleLatest).with_serialization();
    });

    builder.configure::<UserEvent>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
            .with_serialization();
    });

    builder.configure::<Config>(|reg| {
        reg.buffer(BufferCfg::SingleLatest).with_serialization();
    });

    builder.configure::<AppSettings>(|reg| {
        reg.buffer(BufferCfg::SingleLatest).with_serialization();
    });

    let db = builder.build()?;

    info!("✅ Database initialized with 5 record types");
    info!("   - Temperature (has producer, NOT writable)");
    info!("   - SystemStatus (has producer, NOT writable)");
    info!("   - UserEvent (buffer only, no data)");
    info!("   - Config (has producer, NOT writable)");
    info!("   - AppSettings (NO producer, remotely writable ✍️)");

    info!("📝 Populating initial record data...");

    // Produce some initial data
    let temp_producer = db.producer::<Temperature>();
    temp_producer
        .produce(Temperature {
            sensor_id: "sensor-01".to_string(),
            celsius: 22.5,
            timestamp: 1698764400,
        })
        .await?;

    let status_producer = db.producer::<SystemStatus>();
    status_producer
        .produce(SystemStatus {
            uptime_seconds: 3600,
            cpu_usage: 15.3,
            memory_usage: 42.7,
        })
        .await?;

    let config_producer = db.producer::<Config>();
    config_producer
        .produce(Config {
            app_name: "AimDB Demo".to_string(),
            version: "0.1.0".to_string(),
            debug_mode: true,
        })
        .await?;

    // Initialize AppSettings WITHOUT creating a producer
    // This makes it writable via remote access (record.set)
    db.set_record_from_json(
        "server::AppSettings",
        serde_json::json!({
            "log_level": "info",
            "max_connections": 100,
            "feature_flag_alpha": false
        }),
    )?;

    info!("✅ Initial data populated");

    // Spawn background task to continuously update Temperature
    info!("🌡️  Starting live temperature simulator...");
    let temp_producer_clone = temp_producer.clone();
    tokio::spawn(async move {
        let mut counter = 0u64;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            counter += 1;
            let temp = 20.0 + (counter as f64 * 0.5) + (counter as f64 % 10.0);

            let reading = Temperature {
                sensor_id: format!("sensor-{:02}", (counter % 3) + 1),
                celsius: temp,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            };

            if let Err(e) = temp_producer_clone.produce(reading.clone()).await {
                tracing::error!("Failed to produce temperature: {}", e);
            } else {
                tracing::debug!(
                    "📊 Produced temperature: {} °C from {}",
                    reading.celsius,
                    reading.sensor_id
                );
            }
        }
    });

    // Spawn background task to update SystemStatus
    info!("💻 Starting system status simulator...");
    let status_producer_clone = status_producer.clone();
    tokio::spawn(async move {
        let mut uptime = 3600u64;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            uptime += 5;
            let status = SystemStatus {
                uptime_seconds: uptime,
                cpu_usage: 10.0 + (uptime as f64 % 30.0),
                memory_usage: 40.0 + ((uptime as f64 / 10.0) % 20.0),
            };

            if let Err(e) = status_producer_clone.produce(status.clone()).await {
                tracing::error!("Failed to produce system status: {}", e);
            } else {
                tracing::debug!(
                    "📊 Produced system status: CPU {:.1}%, MEM {:.1}%",
                    status.cpu_usage,
                    status.memory_usage
                );
            }
        }
    });

    info!("");
    info!("🎯 Server ready! Connect with:");
    info!("   cargo run --bin client");
    info!("");
    info!("   Or test manually with:");
    info!(
        "   echo '{{\"id\":1,\"method\":\"record.list\"}}' | socat - UNIX-CONNECT:{}",
        socket_path
    );
    info!("");
    info!("📡 Live updates:");
    info!("   Temperature: every 2 seconds");
    info!("   SystemStatus: every 5 seconds");
    info!("");
    info!("Press Ctrl+C to stop the server");

    // Keep server running
    tokio::signal::ctrl_c().await?;

    info!("🛑 Shutting down server...");

    Ok(())
}
