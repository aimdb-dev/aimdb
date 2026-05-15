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
use aimdb_core::{buffer::BufferCfg, AimDbBuilder, Consumer, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

/// Temperature sensor reading
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temperature {
    sensor_id: String,
    celsius: f64,
    timestamp: f64, // Unix timestamp as f64 (seconds since epoch with fractional nanoseconds, e.g., 1730379296.123456789)
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
    security_policy.allow_write_key("server::AppSettings");

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
    // Use SpmcRing for Temperature and SystemStatus to support record.drain history.
    //
    // Temperature and SystemStatus use `.source()` + `.tap()` so AimDB owns the
    // producer/consumer task lifecycle — this also makes them eligible for
    // automatic stage profiling (feature `profiling`, query via the MCP
    // `get_stage_profiling` tool). `.with_name("...")` gives each stage a
    // human-readable label that shows up in the profiling output.
    builder.configure::<Temperature>("server::Temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
            .with_remote_access()
            .source(temperature_simulator)
            .with_name("temp_simulator")
            .tap(temperature_logger)
            .with_name("temp_logger");
    });

    builder.configure::<SystemStatus>("server::SystemStatus", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 50 })
            .with_remote_access()
            .source(system_status_simulator)
            .with_name("status_simulator")
            // Deliberately slow consumer — surfaces as the bottleneck in
            // `get_stage_profiling` for the SystemStatus record.
            .tap(slow_status_processor)
            .with_name("slow_status_processor");
    });

    builder.configure::<UserEvent>("server::UserEvent", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
            .with_remote_access();
    });

    builder.configure::<Config>("server::Config", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    builder.configure::<AppSettings>("server::AppSettings", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    let db = builder.build().await?;

    info!("✅ Database initialized with 5 record types");
    info!("   - Temperature (SpmcRing×100, has producer — drainable 🔄)");
    info!("   - SystemStatus (SpmcRing×50, has producer — drainable 🔄)");
    info!("   - UserEvent (SpmcRing×100, no data)");
    info!("   - Config (SingleLatest, has producer, NOT writable)");
    info!("   - AppSettings (SingleLatest, NO producer, remotely writable ✍️)");

    info!("📝 Populating initial record data...");

    let config_producer = db.producer::<Config>("server::Config");
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
    info!("🌡️  Temperature simulator running via .source() (every 2s)");
    info!("💻 SystemStatus simulator running via .source() (every 5s)");

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
    info!("📈 Stage profiling is enabled. Query it via the aimdb-mcp tools:");
    info!("   get_stage_profiling   record_key=\"Temperature\"   → per-stage timing");
    info!("   reset_stage_profiling                              → clear counters");
    info!("");
    info!("📊 Buffer metrics are enabled. Query via the aimdb-mcp tools:");
    info!("   get_buffer_metrics    record_key=\"SystemStatus\"  → produced/consumed/dropped");
    info!("   reset_buffer_metrics                               → clear counters");
    info!("");
    info!("Press Ctrl+C to stop the server");

    // Keep server running
    tokio::signal::ctrl_c().await?;

    info!("🛑 Shutting down server...");

    Ok(())
}

// ============================================================================
// SOURCES & TAPS — owned by AimDB so the stage-profiling feature can time them
// ============================================================================

/// Periodically produces Temperature readings.
///
/// Because this runs inside a `.source()` callback, every `produce()` call is
/// timed by the `profiling` feature — see `get_stage_profiling`.
async fn temperature_simulator(
    ctx: RuntimeContext<TokioAdapter>,
    temperature: Producer<Temperature, TokioAdapter>,
) {
    let time = ctx.time();
    let mut counter = 0u64;
    loop {
        let celsius = 20.0 + (counter as f64 * 0.5) + (counter as f64 % 10.0);
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let timestamp =
            duration.as_secs() as f64 + duration.subsec_nanos() as f64 / 1_000_000_000.0;

        let reading = Temperature {
            sensor_id: format!("sensor-{:02}", (counter % 3) + 1),
            celsius,
            timestamp,
        };

        if let Err(e) = temperature.produce(reading).await {
            tracing::error!("Failed to produce temperature: {}", e);
        }

        counter += 1;
        time.sleep(time.secs(2)).await;
    }
}

/// Fast tap on Temperature — just logs. Stage profiling will show it as
/// substantially faster than `slow_status_processor` on SystemStatus.
async fn temperature_logger(
    _ctx: RuntimeContext<TokioAdapter>,
    consumer: Consumer<Temperature, TokioAdapter>,
) {
    let Ok(mut reader) = consumer.subscribe() else {
        tracing::error!("Failed to subscribe to Temperature");
        return;
    };
    while let Ok(reading) = reader.recv().await {
        tracing::debug!(
            "🌡️  Logged temperature: {:.1} °C from {}",
            reading.celsius,
            reading.sensor_id
        );
    }
}

/// Periodically produces SystemStatus readings.
async fn system_status_simulator(
    ctx: RuntimeContext<TokioAdapter>,
    status: Producer<SystemStatus, TokioAdapter>,
) {
    let time = ctx.time();
    let mut uptime = 3600u64;
    loop {
        let snapshot = SystemStatus {
            uptime_seconds: uptime,
            cpu_usage: 10.0 + (uptime as f64 % 30.0),
            memory_usage: 40.0 + ((uptime as f64 / 10.0) % 20.0),
        };

        if let Err(e) = status.produce(snapshot).await {
            tracing::error!("Failed to produce system status: {}", e);
        }

        uptime += 5;
        time.sleep(time.secs(5)).await;
    }
}

/// Intentionally slow tap on SystemStatus — sleeps 100ms per value to simulate
/// expensive per-value processing. With profiling enabled, this stage shows up
/// as the per-record bottleneck in `get_stage_profiling`.
async fn slow_status_processor(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: Consumer<SystemStatus, TokioAdapter>,
) {
    let time = ctx.time();
    let Ok(mut reader) = consumer.subscribe() else {
        tracing::error!("Failed to subscribe to SystemStatus");
        return;
    };
    while let Ok(snapshot) = reader.recv().await {
        time.sleep(time.millis(100)).await;
        tracing::debug!(
            "💻 Processed status: CPU {:.1}%, MEM {:.1}%",
            snapshot.cpu_usage,
            snapshot.memory_usage
        );
    }
}
