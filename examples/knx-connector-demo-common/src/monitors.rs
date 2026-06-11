//! Runtime-Agnostic Monitor Functions
//!
//! These monitor functions work with any AimDB runtime adapter (Tokio or Embassy).
//! They demonstrate the "write once, run anywhere" philosophy.

#[cfg(feature = "alloc")]
extern crate alloc;

use crate::types::{LightState, TemperatureReading};
use aimdb_core::{Consumer, RuntimeContext};

// ============================================================================
// TEMPERATURE MONITOR
// ============================================================================

/// Temperature monitor that logs readings from the buffer
///
/// Uses the location from the `TemperatureReading` data itself.
///
/// # Example
/// ```ignore
/// builder.configure::<TemperatureReading>("temp.livingroom", |reg| {
///     reg.buffer(...)
///        .tap(temperature_monitor)
///        .link_from("knx://9/1/0")
///        .finish();
/// });
/// ```
pub async fn temperature_monitor(ctx: RuntimeContext, consumer: Consumer<TemperatureReading>) {
    let log = ctx.log();
    log.info("🌡️  Temperature monitor started\n");

    let mut reader = consumer.subscribe();

    while let Ok(temp) = reader.recv().await {
        let (whole, frac) = temp.display_parts();
        log.info(&alloc::format!(
            "🌡️  {}: {}.{}°C",
            temp.location,
            whole,
            frac
        ));
    }
}

// ============================================================================
// LIGHT MONITOR
// ============================================================================

/// Light monitor that logs state changes from the buffer
///
/// Uses the group address from the `LightState` data itself.
pub async fn light_monitor(ctx: RuntimeContext, consumer: Consumer<LightState>) {
    let log = ctx.log();
    log.info("💡 Light monitor started\n");

    let mut reader = consumer.subscribe();

    while let Ok(state) = reader.recv().await {
        log.info(&alloc::format!(
            "💡 Light {}: {}",
            state.group_address,
            state.state_display()
        ));
    }
}
