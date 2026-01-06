//! Simulation configuration for data contracts.
//!
//! These structures configure how `Simulatable` types generate test data.

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════
// SIMULATION CONFIG
// ═══════════════════════════════════════════════════════════════════

/// Configuration for data simulation.
///
/// This is passed to `Simulatable::simulate()` to control how
/// test data is generated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Whether simulation is active
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Interval between simulated samples (milliseconds)
    #[serde(default = "default_interval")]
    pub interval_ms: u64,

    /// Type-specific parameters (schema implementations interpret these)
    #[serde(default)]
    pub params: SimulationParams,
}

fn default_true() -> bool {
    true
}

fn default_interval() -> u64 {
    1000
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 1000,
            params: SimulationParams::default(),
        }
    }
}

/// Type-specific simulation parameters.
///
/// Common parameters that many schema types use. Schema implementations
/// can interpret these as appropriate for their domain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationParams {
    /// Base/center value for the simulation
    #[serde(default)]
    pub base: f64,

    /// Maximum deviation from base (for random walks)
    #[serde(default = "default_variation")]
    pub variation: f64,

    /// Linear trend per sample (positive = increasing, negative = decreasing)
    #[serde(default)]
    pub trend: f64,

    /// Step size multiplier for random walk (0.0-1.0)
    #[serde(default = "default_step")]
    pub step: f64,
}

fn default_variation() -> f64 {
    1.0
}

fn default_step() -> f64 {
    0.2
}

impl Default for SimulationParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            variation: 1.0,
            trend: 0.0,
            step: 0.2,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// TESTS
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulation_config_defaults() {
        let config = SimulationConfig::default();
        assert!(config.enabled);
        assert_eq!(config.interval_ms, 1000);
        assert_eq!(config.params.base, 0.0);
        assert_eq!(config.params.variation, 1.0);
        assert_eq!(config.params.step, 0.2);
    }

    #[test]
    fn test_simulation_config_json_roundtrip() {
        let json = r#"{
            "enabled": true,
            "interval_ms": 500,
            "params": {
                "base": 22.0,
                "variation": 3.0,
                "trend": 0.1,
                "step": 0.1
            }
        }"#;

        let config: SimulationConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.interval_ms, 500);
        assert_eq!(config.params.base, 22.0);
        assert_eq!(config.params.variation, 3.0);
        assert_eq!(config.params.trend, 0.1);

        // Roundtrip
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: SimulationConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }
}
