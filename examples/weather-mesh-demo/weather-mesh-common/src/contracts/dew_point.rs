//! Dew point derived measurement
//!
//! Computed from `Temperature` and `Humidity` via the Magnus approximation:
//! `T_dp ≈ T_celsius - (100 - RH_percent) / 5`
//!
//! Accurate to ±1°C for RH > 50%. Requires only basic f32 arithmetic — no libm.

extern crate alloc;

use aimdb_data_contracts::{Observable, SchemaType, Streamable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use aimdb_data_contracts::Linkable;

/// Dew point temperature derived from `Temperature` and `Humidity`.
///
/// Not sensed directly — produced by a `transform_join` over
/// [`super::Temperature`] and [`super::Humidity`] records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DewPoint {
    /// Dew point in degrees Celsius
    pub celsius: f32,
    /// Unix timestamp (ms) of the most recent contributing sensor reading
    pub timestamp: u64,
}

impl SchemaType for DewPoint {
    const NAME: &'static str = "dew_point";
}

impl Streamable for DewPoint {}

impl Observable for DewPoint {
    type Signal = f32;
    const ICON: &'static str = "🌫️";
    const UNIT: &'static str = "°C";

    fn signal(&self) -> f32 {
        self.celsius
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        alloc::format!(
            "{} [{}] DewPoint: {:.1}{} at {}",
            Self::ICON,
            node_id,
            self.celsius,
            Self::UNIT,
            self.timestamp
        )
    }
}

#[cfg(feature = "linkable")]
impl Linkable for DewPoint {
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        serde_json::from_slice(data).map_err(|e| alloc::string::ToString::to_string(&e))
    }

    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        serde_json::to_vec(self).map_err(|e| alloc::string::ToString::to_string(&e))
    }
}
