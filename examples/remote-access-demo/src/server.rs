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

/// Configuration settings
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    app_name: String,
    version: String,
    debug_mode: bool,
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

    let remote_config = AimxConfig::uds_default()
        .socket_path(socket_path)
        .security_policy(SecurityPolicy::ReadOnly)
        .max_connections(10)
        .subscription_queue_size(100);

    info!("📡 Remote access will be available at: {}", socket_path);
    info!("🔒 Security policy: ReadOnly");

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

    let db = builder.build()?;

    info!("✅ Database initialized with 4 record types");
    info!("   - Temperature");
    info!("   - SystemStatus");
    info!("   - UserEvent");
    info!("   - Config");

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

    info!("✅ Initial data populated");
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
    info!("Press Ctrl+C to stop the server");

    // Keep server running
    tokio::signal::ctrl_c().await?;

    info!("🛑 Shutting down server...");

    Ok(())
}
