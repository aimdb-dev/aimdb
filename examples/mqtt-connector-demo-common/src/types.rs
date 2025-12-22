//! Shared Data Types for MQTT Demo
//!
//! These types work in both `std` and `no_std` environments using `alloc::string::String`.

extern crate alloc;

use alloc::string::String;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

// ============================================================================
// TEMPERATURE READING
// ============================================================================

/// Temperature reading from a sensor
///
/// Represents a temperature measurement that can be published to MQTT.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct Temperature {
    /// Sensor identifier (e.g., "indoor-001", "outdoor-001")
    pub sensor_id: String,
    /// Temperature in degrees Celsius
    pub celsius: f32,
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

    /// Serialize to JSON format (works on both std and no_std)
    pub fn to_json_vec(&self) -> alloc::vec::Vec<u8> {
        #[cfg(feature = "std")]
        {
            serde_json::to_vec(self).unwrap_or_default()
        }
        #[cfg(not(feature = "std"))]
        {
            use alloc::format;
            let celsius_int = (self.celsius * 10.0) as i32;
            format!(
                r#"{{"sensor_id":"{}","celsius_x10":{}}}"#,
                self.sensor_id, celsius_int
            )
            .into_bytes()
        }
    }
}

// ============================================================================
// TEMPERATURE COMMAND
// ============================================================================

/// Command for controlling temperature sensors (inbound from MQTT)
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct TemperatureCommand {
    /// Action to perform (e.g., "read", "calibrate", "reset")
    pub action: String,
    /// Target sensor identifier
    pub sensor_id: String,
}

impl TemperatureCommand {
    /// Create a new temperature command
    pub fn new(action: &str, sensor_id: &str) -> Self {
        Self {
            action: action.into(),
            sensor_id: sensor_id.into(),
        }
    }

    /// Parse from JSON bytes (works on both std and no_std)
    pub fn from_json(data: &[u8]) -> Result<Self, alloc::string::String> {
        #[cfg(feature = "std")]
        {
            serde_json::from_slice(data)
                .map_err(|e| alloc::format!("Failed to deserialize command: {}", e))
        }
        #[cfg(not(feature = "std"))]
        {
            Self::parse_json_no_std(data)
        }
    }

    /// Simple JSON parser for no_std environments
    #[cfg(not(feature = "std"))]
    fn parse_json_no_std(data: &[u8]) -> Result<Self, alloc::string::String> {
        use alloc::string::ToString;
        use alloc::vec::Vec;

        let text = core::str::from_utf8(data).map_err(|_| "Invalid UTF-8".to_string())?;

        let mut action = String::new();
        let mut sensor_id = String::new();

        for pair in text.trim_matches(|c| c == '{' || c == '}').split(',') {
            let parts: Vec<&str> = pair.split(':').collect();
            if parts.len() != 2 {
                continue;
            }

            let key = parts[0].trim().trim_matches('"');
            let value = parts[1].trim().trim_matches('"');

            match key {
                "action" => action = value.into(),
                "sensor_id" => sensor_id = value.into(),
                _ => {}
            }
        }

        if action.is_empty() || sensor_id.is_empty() {
            return Err("Missing required fields".to_string());
        }

        Ok(TemperatureCommand { action, sensor_id })
    }
}
