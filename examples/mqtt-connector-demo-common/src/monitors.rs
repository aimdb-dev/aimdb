//! Runtime-Agnostic Monitor Functions
//!
//! These monitor functions work with any AimDB runtime adapter (Tokio or Embassy).
//! They demonstrate the "write once, run anywhere" philosophy.

extern crate alloc;

use crate::types::{Temperature, TemperatureCommand};
use aimdb_core::{Consumer, RuntimeContext, Stamp};

/// A write stamp as `@ <seconds>.<millis>s` since the instance started.
/// Integer formatting only, so it stays cheap on embedded targets.
fn at(stamp: Stamp) -> alloc::string::String {
    let ms = stamp.as_nanos() / 1_000_000;
    alloc::format!("@ {}.{:03}s", ms / 1000, ms % 1000)
}

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
pub async fn temperature_logger(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let log = ctx.log();

    let mut reader = consumer.subscribe();

    while let Ok(temp) = reader.recv().await {
        log.info(&alloc::format!(
            "{} Temperature logged: {:.1}°C from {} {}",
            temp.value.icon(),
            temp.value.celsius,
            temp.value.sensor_id,
            at(temp.stamp())
        ));
    }
}

// ============================================================================
// COMMAND CONSUMER
// ============================================================================

/// Command consumer that logs received commands
///
/// Uses the action and sensor_id from the `TemperatureCommand` data.
pub async fn command_consumer(ctx: RuntimeContext, consumer: Consumer<TemperatureCommand>) {
    let log = ctx.log();
    log.info("📨 Command consumer started\n");

    let mut reader = consumer.subscribe();

    while let Ok(cmd) = reader.recv().await {
        log.info(&alloc::format!(
            "📨 Command received: action='{}' sensor_id='{}' {}",
            cmd.value.action,
            cmd.value.sensor_id,
            at(cmd.stamp())
        ));
    }
}
