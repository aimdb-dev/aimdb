//! Shared Data Types for KNX Demo
//!
//! These types work in both `std` and `no_std` environments using `alloc::string::String`.

extern crate alloc;

use alloc::string::String;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

// ============================================================================
// TEMPERATURE READING (DPT 9.001)
// ============================================================================

/// Temperature reading from a KNX sensor
///
/// Uses DPT 9.001 (2-byte floating point) for temperature values.
/// The location field identifies which sensor this reading came from.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct TemperatureReading {
    /// Sensor location identifier (e.g., "Living Room", "Kitchen")
    pub location: String,
    /// Temperature in degrees Celsius
    pub celsius: f32,
}

impl TemperatureReading {
    /// Create a new temperature reading
    pub fn new(location: &str, celsius: f32) -> Self {
        Self {
            location: location.into(),
            celsius,
        }
    }

    /// Get the whole and fractional parts for embedded display
    /// Returns (whole_part, fractional_tenths)
    pub fn display_parts(&self) -> (i32, u32) {
        let whole = self.celsius as i32;
        let frac = ((self.celsius.abs() - (whole.abs() as f32)) * 10.0) as u32;
        (whole, frac)
    }
}

// ============================================================================
// LIGHT STATE (DPT 1.001 - Inbound)
// ============================================================================

/// Light switch state received from KNX bus
///
/// Uses DPT 1.001 (1-bit boolean) for switch state.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct LightState {
    /// KNX group address this state came from (e.g., "1/0/7")
    pub group_address: String,
    /// Whether the light is on
    pub is_on: bool,
}

impl LightState {
    /// Create a new light state
    pub fn new(group_address: &str, is_on: bool) -> Self {
        Self {
            group_address: group_address.into(),
            is_on,
        }
    }

    /// Get display string for the state
    pub fn state_display(&self) -> &'static str {
        if self.is_on { "ON ✨" } else { "OFF" }
    }
}

// ============================================================================
// LIGHT CONTROL (DPT 1.001 - Outbound)
// ============================================================================

/// Light control command to send to KNX bus
///
/// Uses DPT 1.001 (1-bit boolean) for switch command.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct LightControl {
    /// Target KNX group address (e.g., "1/0/6")
    pub group_address: String,
    /// Desired state: true = on, false = off
    pub is_on: bool,
}

impl LightControl {
    /// Create a new light control command
    pub fn new(group_address: &str, is_on: bool) -> Self {
        Self {
            group_address: group_address.into(),
            is_on,
        }
    }

    /// Get display string for the command
    pub fn state_display(&self) -> &'static str {
        if self.is_on { "ON ✨" } else { "OFF" }
    }
}

// ============================================================================
// KNX GROUP ADDRESS CONSTANTS
// ============================================================================

/// Common KNX group addresses used in the demo
pub mod addresses {
    // Temperature sensors (DPT 9.001)
    pub const TEMP_LIVING_ROOM: &str = "9/1/0";
    pub const TEMP_BEDROOM: &str = "9/0/1";
    pub const TEMP_KITCHEN: &str = "9/1/2";

    // Light status feedback (DPT 1.001)
    pub const LIGHT_MAIN_STATUS: &str = "1/0/7";
    pub const LIGHT_HALLWAY_STATUS: &str = "1/0/8";

    // Light control (DPT 1.001)
    pub const LIGHT_CONTROL: &str = "1/0/6";
}
