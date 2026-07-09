//! Humidity sensor schema

extern crate alloc;

use aimdb_data_contracts::{Observable, SchemaType, Settable, Streamable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use aimdb_data_contracts::Linkable;

#[cfg(feature = "simulatable")]
use aimdb_data_contracts::{RandomWalkParams, Simulatable};
#[cfg(feature = "simulatable")]
use rand::RngExt;

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
    const UNIT: &'static str = "%";

    fn signal(&self) -> f32 {
        self.percent
    }
}

#[cfg(feature = "simulatable")]
impl Simulatable for Humidity {
    type Params = RandomWalkParams;

    /// Simulate humidity readings with random walk behavior.
    ///
    /// # Params interpretation
    /// - `base`: Center humidity value (default: 50.0%)
    /// - `variation`: Maximum deviation from base (default: 10.0%)
    /// - `step`: Random walk step multiplier (default: 0.2)
    /// - `trend`: Linear trend per sample (default: 0.0)
    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self {
        let base = params.base as f32;
        let variation = params.variation as f32;
        let step = params.step as f32;
        let trend = params.trend as f32;

        // Random walk: small delta from previous value, clamped to valid range
        let current = match previous {
            Some(prev) => {
                let delta = (rng.random::<f32>() - 0.5) * variation * step;
                (prev.percent + delta + trend)
                    .clamp(0.0, 100.0)
                    .clamp(base - variation, base + variation)
            }
            None => base + (rng.random::<f32>() - 0.5) * variation,
        };

        Humidity {
            percent: current,
            timestamp: timestamp_ms,
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
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        serde_json::from_slice(data).map_err(|e| alloc::string::ToString::to_string(&e))
    }

    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        serde_json::to_vec(self).map_err(|e| alloc::string::ToString::to_string(&e))
    }
}
