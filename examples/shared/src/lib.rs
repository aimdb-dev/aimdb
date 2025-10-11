//! Shared service implementations for AimDB examples
//!
//! These services are runtime-agnostic and can be used with any Runtime implementation
//! (TokioAdapter, EmbassyAdapter, etc.)
//!
//! # Buffer Services
//!
//! This module demonstrates three buffer patterns:
//!
//! 1. **SPMC Ring Buffer** (`telemetry_service`):
//!    - High-frequency sensor data
//!    - Multiple consumers with lag detection
//!    - Bounded capacity for backpressure
//!
//! 2. **SingleLatest** (`config_service`):
//!    - Configuration updates
//!    - Consumers only care about current value
//!    - Intermediate updates are skipped
//!
//! 3. **Mailbox** (`command_service`):
//!    - Command processing
//!    - Single consumer, overwrite old unprocessed commands
//!    - Latest command wins

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::format;

use aimdb_core::{buffer::BufferReader, DbResult, RuntimeContext};
use aimdb_executor::Runtime;

//
// ============================================================================
// BUFFER DEMONSTRATION SERVICES
// ============================================================================
//

/// Telemetry data structure for sensor readings
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(PartialEq))]
pub struct TelemetryReading {
    pub sensor_id: u32,
    pub temperature: i32, // Temperature in Celsius * 100 (e.g., 2350 = 23.50°C)
    pub humidity: u32,    // Humidity percentage * 100 (e.g., 5500 = 55.00%)
}

/// Configuration update structure
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(PartialEq))]
pub struct ConfigUpdate {
    pub version: u32,
    pub sampling_rate_hz: u32,
    pub enable_alerts: bool,
}

/// Command structure for device control
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(PartialEq))]
pub struct DeviceCommand {
    pub command_id: u32,
    pub action: CommandAction,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(PartialEq))]
pub enum CommandAction {
    StartSampling,
    StopSampling,
    Reset,
    Calibrate,
}

/// Telemetry producer service - generates high-frequency sensor data
///
/// Demonstrates SPMC Ring Buffer usage:
/// - Produces data rapidly
/// - Multiple consumers can read at different rates
/// - Lag detection when consumers fall behind
pub async fn telemetry_producer_service<R: Runtime>(
    ctx: RuntimeContext<R>,
    count: u32,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("📡 Telemetry producer starting");

    for i in 0..count {
        let reading = TelemetryReading {
            sensor_id: 1,
            temperature: 2300 + (i as i32 % 500), // 23-28°C range
            humidity: 5000 + (i % 2000),          // 50-70% range
        };

        // In real code, this would come from TypedRecord::produce()
        // For this demo, we're just showing the data structure
        log.info(&format!(
            "📊 Telemetry #{}: temp={}.{:02}°C humidity={}.{:02}%",
            i,
            reading.temperature / 100,
            reading.temperature % 100,
            reading.humidity / 100,
            reading.humidity % 100
        ));

        // Simulate high-frequency sampling
        time.sleep(time.millis(50)).await;
    }

    log.info("📡 Telemetry producer completed");
    Ok(())
}

/// Telemetry consumer service - processes sensor data from SPMC buffer
///
/// Demonstrates buffer reader usage with lag handling
pub async fn telemetry_consumer_service<R: Runtime, Reader: BufferReader<TelemetryReading>>(
    ctx: RuntimeContext<R>,
    mut reader: Reader,
    consumer_id: u32,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("📥 Telemetry consumer starting");

    loop {
        match reader.recv().await {
            Ok(reading) => {
                log.info(&format!(
                    "Consumer {} received: sensor={} temp={}.{:02}°C",
                    consumer_id,
                    reading.sensor_id,
                    reading.temperature / 100,
                    reading.temperature % 100
                ));

                // Simulate processing time
                time.sleep(time.millis(100)).await;
            }
            Err(aimdb_core::DbError::BufferLagged { lag_count, .. }) => {
                log.info(&format!(
                    "⚠️  Consumer {} lagged: skipped {} messages",
                    consumer_id, lag_count
                ));

                // Continue processing after lag
                continue;
            }
            Err(aimdb_core::DbError::BufferClosed { .. }) => {
                log.info("Buffer closed, consumer exiting");
                break;
            }
            Err(_) => {
                log.info("Consumer error, exiting");
                break;
            }
        }
    }

    log.info("📥 Telemetry consumer completed");
    Ok(())
}

/// Configuration producer service - publishes config updates
///
/// Demonstrates SingleLatest buffer usage:
/// - Updates published rapidly
/// - Consumers only see latest value
/// - Intermediate updates are skipped automatically
pub async fn config_producer_service<R: Runtime>(
    ctx: RuntimeContext<R>,
    updates: u32,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("⚙️  Config producer starting");

    for version in 1..=updates {
        let config = ConfigUpdate {
            version,
            sampling_rate_hz: 10 * version, // Increasing sampling rate
            enable_alerts: version % 2 == 0,
        };

        log.info(&format!(
            "📝 Config v{}: rate={}Hz alerts={}",
            config.version, config.sampling_rate_hz, config.enable_alerts
        ));

        // Rapid updates to demonstrate skipping
        time.sleep(time.millis(30)).await;
    }

    log.info("⚙️  Config producer completed");
    Ok(())
}

/// Configuration consumer service - applies config updates from SingleLatest buffer
///
/// Demonstrates how SingleLatest automatically skips intermediate values
pub async fn config_consumer_service<R: Runtime, Reader: BufferReader<ConfigUpdate>>(
    ctx: RuntimeContext<R>,
    mut reader: Reader,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("📖 Config consumer starting");

    loop {
        match reader.recv().await {
            Ok(config) => {
                log.info(&format!(
                    "✅ Applied config v{}: rate={}Hz",
                    config.version, config.sampling_rate_hz
                ));

                // Slow processing to demonstrate skipping
                time.sleep(time.millis(100)).await;
            }
            Err(aimdb_core::DbError::BufferClosed { .. }) => {
                log.info("Config buffer closed, consumer exiting");
                break;
            }
            Err(_) => {
                log.info("Config consumer error, exiting");
                break;
            }
        }
    }

    log.info("📖 Config consumer completed");
    Ok(())
}

/// Command producer service - sends device commands
///
/// Demonstrates Mailbox buffer usage:
/// - Commands sent rapidly
/// - Only latest unread command is kept
/// - Old commands are overwritten if not consumed yet
pub async fn command_producer_service<R: Runtime>(
    ctx: RuntimeContext<R>,
    commands: u32,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🎮 Command producer starting");

    let actions = [
        CommandAction::StartSampling,
        CommandAction::StopSampling,
        CommandAction::Calibrate,
        CommandAction::Reset,
    ];

    for i in 0..commands {
        let cmd = DeviceCommand {
            command_id: i,
            action: actions[(i % 4) as usize].clone(),
        };

        log.info(&format!("🎯 Command #{}: {:?}", cmd.command_id, cmd.action));

        // Send commands faster than they can be processed
        time.sleep(time.millis(20)).await;
    }

    log.info("🎮 Command producer completed");
    Ok(())
}

/// Command consumer service - executes device commands from Mailbox buffer
///
/// Demonstrates how Mailbox overwrites unread commands
pub async fn command_consumer_service<R: Runtime, Reader: BufferReader<DeviceCommand>>(
    ctx: RuntimeContext<R>,
    mut reader: Reader,
) -> DbResult<()> {
    let log = ctx.log();
    let time = ctx.time();

    log.info("⚡ Command consumer starting");

    loop {
        match reader.recv().await {
            Ok(cmd) => {
                log.info(&format!(
                    "✨ Executing command #{}: {:?}",
                    cmd.command_id, cmd.action
                ));

                // Simulate slow command execution
                time.sleep(time.millis(100)).await;
            }
            Err(aimdb_core::DbError::BufferClosed { .. }) => {
                log.info("Command buffer closed, consumer exiting");
                break;
            }
            Err(_) => {
                log.info("Command consumer error, exiting");
                break;
            }
        }
    }

    log.info("⚡ Command consumer completed");
    Ok(())
}
