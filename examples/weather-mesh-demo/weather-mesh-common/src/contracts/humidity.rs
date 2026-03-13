//! Humidity sensor schema

extern crate alloc;

use aimdb_data_contracts::{Observable, SchemaType, Settable, Streamable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use aimdb_data_contracts::Linkable;

#[cfg(feature = "simulatable")]
use aimdb_data_contracts::{Simulatable, SimulationConfig};

/// Humidity sensor reading
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Humidity {
    /// Relative humidity as a percentage (0-100)
    pub percent: f32,
    /// Unix timestamp (milliseconds) when reading was taken
    pub timestamp: u64,
}

impl SchemaType for Humidity {
    const NAME: &'static str = "humidity";
}

impl Streamable for Humidity {}

impl Observable for Humidity {
    type Signal = f32;
    const ICON: &'static str = "💧";
    const UNIT: &'static str = "%";

    fn signal(&self) -> f32 {
        self.percent
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        alloc::format!(
            "{} [{}] Humidity: {:.1}{} at {}",
            Self::ICON,
            node_id,
            self.percent,
            Self::UNIT,
            self.timestamp
        )
    }
}

#[cfg(feature = "simulatable")]
impl Simulatable for Humidity {
    /// Simulate humidity readings with random walk behavior.
    ///
    /// # Config params interpretation
    /// - `base`: Center humidity value (default: 50.0%)
    /// - `variation`: Maximum deviation from base (default: 10.0%)
    /// - `step`: Random walk step multiplier (default: 0.2)
    /// - `trend`: Linear trend per sample (default: 0.0)
    fn simulate<R: rand::Rng>(
        config: &SimulationConfig,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp: u64,
    ) -> Self {
        let base = config.params.base as f32;
        let variation = config.params.variation as f32;
        let step = config.params.step as f32;
        let trend = config.params.trend as f32;

        // Random walk: small delta from previous value, clamped to valid range
        let current = match previous {
            Some(prev) => {
                let delta = (rng.gen::<f32>() - 0.5) * variation * step;
                (prev.percent + delta + trend)
                    .clamp(0.0, 100.0)
                    .clamp(base - variation, base + variation)
            }
            None => base + (rng.gen::<f32>() - 0.5) * variation,
        };

        Humidity {
            percent: current,
            timestamp,
        }
    }
}

impl Settable for Humidity {
    type Value = f32;

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Humidity {
            percent: value,
            timestamp,
        }
    }
}

#[cfg(feature = "linkable")]
impl Linkable for Humidity {
    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(data).map_err(|e| e.to_string())
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }
}
