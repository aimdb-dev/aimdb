//! Runtime-Agnostic Monitor Functions
//!
//! These monitor functions work with any AimDB runtime adapter (Tokio or Embassy).
//! They demonstrate the "write once, run anywhere" philosophy.

extern crate alloc;

use crate::types::{Temperature, TemperatureCommand};
use aimdb_core::{Consumer, Logger, Runtime, RuntimeContext};

// ============================================================================
// TEMPERATURE LOGGER
// ============================================================================

/// Temperature logger that logs readings from the buffer
///
/// Uses the sensor_id from the `Temperature` data to determine the icon.
///
/// # Example
/// ```ignore
/// builder.configure::<Temperature>("sensor.temp.indoor", |reg| {
///     reg.buffer(...)
///        .tap(temperature_logger)
///        .link_to("mqtt://sensors/temp/indoor")
///        .finish();
/// });
/// ```
pub async fn temperature_logger<R>(ctx: RuntimeContext<R>, consumer: Consumer<Temperature>)
where
    R: Runtime + Logger + Send + Sync + 'static,
{
    let log = ctx.log();

    let mut reader = consumer.subscribe();

    while let Ok(temp) = reader.recv().await {
        log.info(&alloc::format!(
            "{} Temperature logged: {:.1}°C from {}",
            temp.icon(),
            temp.celsius,
            temp.sensor_id
        ));
    }
}

// ============================================================================
// COMMAND CONSUMER
// ============================================================================

/// Command consumer that logs received commands
///
/// Uses the action and sensor_id from the `TemperatureCommand` data.
pub async fn command_consumer<R>(ctx: RuntimeContext<R>, consumer: Consumer<TemperatureCommand>)
where
    R: Runtime + Logger + Send + Sync + 'static,
{
    let log = ctx.log();
    log.info("📨 Command consumer started\n");

    let mut reader = consumer.subscribe();

    while let Ok(cmd) = reader.recv().await {
        log.info(&alloc::format!(
            "📨 Command received: action='{}' sensor_id='{}'",
            cmd.action,
            cmd.sensor_id
        ));
    }
}
