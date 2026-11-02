//! Shared Data Types for MQTT Demo
//!
//! The demo's contracts. They work in both `std` and `no_std` environments
//! using `alloc::string::String`, and their wire format is JSON through
//! `#[derive(Linkable)]` on every target, so `publish` / `subscribe` and
//! remote access use the same encoding on Tokio and Embassy.

extern crate alloc;

use alloc::string::String;

use aimdb_data_contracts::{Linkable, SchemaType};
use serde::{Deserialize, Serialize};

// ============================================================================
// TEMPERATURE READING
// ============================================================================

/// Temperature reading from a sensor
///
/// Represents a temperature measurement that can be published to MQTT.
#[derive(Clone, Debug, Serialize, Deserialize, Linkable)]
pub struct Temperature {
    /// Sensor identifier (e.g., "indoor-001", "outdoor-001")
    pub sensor_id: String,
    /// Temperature in degrees Celsius
    pub celsius: f32,
}

impl SchemaType for Temperature {
    const NAME: &'static str = "mqtt_demo.temperature";
}

impl Temperature {
    /// Create a new temperature reading
    pub fn new(sensor_id: &str, celsius: f32) -> Self {
        Self {
            sensor_id: sensor_id.into(),
            celsius,
        }
    }

    /// Get icon based on sensor type
    pub fn icon(&self) -> &'static str {
        if self.sensor_id.starts_with("indoor") {
            "🏠"
        } else if self.sensor_id.starts_with("outdoor") {
            "🌳"
        } else if self.sensor_id.starts_with("server") {
            "🖥️"
        } else {
            "📊"
        }
    }
}

// ============================================================================
// TEMPERATURE COMMAND
// ============================================================================

/// Command for controlling temperature sensors (inbound from MQTT)
#[derive(Clone, Debug, Serialize, Deserialize, Linkable)]
pub struct TemperatureCommand {
    /// Action to perform (e.g., "read", "calibrate", "reset")
    pub action: String,
    /// Target sensor identifier
    pub sensor_id: String,
}

impl SchemaType for TemperatureCommand {
    const NAME: &'static str = "mqtt_demo.temperature_command";
}

impl TemperatureCommand {
    /// Create a new temperature command
    pub fn new(action: &str, sensor_id: &str) -> Self {
        Self {
            action: action.into(),
            sensor_id: sensor_id.into(),
        }
    }
}
