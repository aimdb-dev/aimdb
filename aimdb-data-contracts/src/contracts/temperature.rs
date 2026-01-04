//! Temperature sensor schema

use crate::{Observable, SchemaType, Settable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use crate::Linkable;

#[cfg(feature = "simulation")]
use crate::{Simulatable, SimulationConfig};

/// Temperature sensor reading in Celsius
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Temperature {
    /// Temperature in degrees Celsius
    pub celsius: f32,
    /// Unix timestamp (milliseconds) when reading was taken
    pub timestamp: u64,
}

impl SchemaType for Temperature {
    const NAME: &'static str = "temperature";
}

impl Observable for Temperature {
    type Signal = f32;

    fn signal(&self) -> f32 {
        self.celsius
    }
}

#[cfg(feature = "simulation")]
impl Simulatable for Temperature {
    /// Simulate temperature readings with random walk behavior.
    ///
    /// # Config params interpretation
    /// - `base`: Center temperature value (default: 22.0°C)
    /// - `variation`: Maximum deviation from base (default: 3.0°C)
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

        // Random walk: small delta from previous value, clamped to range
        let current = match previous {
            Some(prev) => {
                let delta = (rng.gen::<f32>() - 0.5) * variation * step;
                (prev.celsius + delta + trend).clamp(base - variation, base + variation)
            }
            None => base + (rng.gen::<f32>() - 0.5) * variation,
        };

        Temperature {
            celsius: current,
            timestamp,
        }
    }
}

impl Settable for Temperature {
    type Value = f32;

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Temperature {
            celsius: value,
            timestamp,
        }
    }
}

#[cfg(feature = "linkable")]
impl Linkable for Temperature {
    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(data).map_err(|e| e.to_string())
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settable() {
        let temp = Temperature::set(22.5, 1704326400000);
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[test]
    fn test_schema_name() {
        assert_eq!(Temperature::NAME, "temperature");
    }

    #[cfg(feature = "simulation")]
    #[test]
    fn test_simulation() {
        use crate::simulatable::SimulationParams;
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let config = SimulationConfig {
            enabled: true,
            interval_ms: 1000,
            params: SimulationParams {
                base: 20.0,
                variation: 5.0,
                trend: 0.0,
                step: 0.2,
            },
        };

        let mut rng = StdRng::seed_from_u64(42);

        // Generate first sample
        let temp1 = Temperature::simulate(&config, None, &mut rng, 1000);
        assert!(temp1.celsius >= 15.0 && temp1.celsius <= 25.0);

        // Generate second sample (should be close to first due to random walk)
        let temp2 = Temperature::simulate(&config, Some(&temp1), &mut rng, 2000);
        let diff = (temp2.celsius - temp1.celsius).abs();
        assert!(diff < 1.0, "Random walk step too large: {}", diff);
    }

    #[cfg(feature = "simulation")]
    #[test]
    fn test_simulation_with_trend() {
        use crate::simulatable::SimulationParams;
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let config = SimulationConfig {
            enabled: true,
            interval_ms: 1000,
            params: SimulationParams {
                base: 20.0,
                variation: 10.0,
                trend: 0.5, // Strong upward trend
                step: 0.0,  // No random walk, just trend
            },
        };

        let mut rng = StdRng::seed_from_u64(42);

        let temp1 = Temperature::simulate(&config, None, &mut rng, 1000);
        let temp2 = Temperature::simulate(&config, Some(&temp1), &mut rng, 2000);
        let temp3 = Temperature::simulate(&config, Some(&temp2), &mut rng, 3000);

        // With positive trend and no randomness, each should be higher
        assert!(
            temp2.celsius > temp1.celsius,
            "Trend should increase temperature"
        );
        assert!(
            temp3.celsius > temp2.celsius,
            "Trend should increase temperature"
        );
    }
}
