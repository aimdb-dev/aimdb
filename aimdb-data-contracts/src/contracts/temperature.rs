//! Temperature sensor schema
//!
//! # Schema Evolution
//!
//! This module demonstrates backward-compatible schema migration with
//! **version-aware payloads** for decoupled deployment:
//!
//! - **v1** (legacy): `{ "schema_version": 1, "temp": f32, "timestamp": u64, "unit": "C"|"F"|"K" }`
//! - **v2** (current): `{ "schema_version": 2, "celsius": f32, "timestamp": u64 }`
//!
//! The `MigrationChain` impl (via `migration_chain!`) reads the `schema_version`
//! from the payload and migrates automatically, allowing nodes and hubs to be updated independently.

extern crate alloc;

use crate::{Observable, SchemaType, Settable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use crate::Linkable;

#[cfg(feature = "simulatable")]
use crate::{Simulatable, SimulationConfig};

#[cfg(feature = "migratable")]
use crate::{MigrationError, MigrationStep};

#[cfg(feature = "ts")]
use ts_rs::TS;

/// Temperature sensor reading in Celsius
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(TS))]
#[cfg_attr(feature = "ts", ts(export))]
pub struct Temperature {
    /// Schema version (always 2 for current format)
    #[serde(default = "default_v2_version")]
    pub schema_version: u32,
    /// Temperature in degrees Celsius
    pub celsius: f32,
    /// Unix timestamp (milliseconds) when reading was taken
    pub timestamp: u64,
}

fn default_v2_version() -> u32 {
    2
}

impl Temperature {
    /// Create a new Temperature reading (current schema version)
    pub fn new(celsius: f32, timestamp: u64) -> Self {
        Self {
            schema_version: 2,
            celsius,
            timestamp,
        }
    }
}

impl SchemaType for Temperature {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2; // v2: celsius + timestamp (v1 had temp + timestamp + unit)
}

// ═══════════════════════════════════════════════════════════════════
// LEGACY v1 SCHEMA (for migration purposes)
// ═══════════════════════════════════════════════════════════════════

/// Legacy Temperature schema (v1) - for migration from old nodes.
///
/// v1 format: `{ "schema_version": 1, "temp": f32, "timestamp": u64, "unit": "C"|"F"|"K" }`
///
/// This is kept in the contracts crate so migration logic can be tested
/// in CI/CD, ensuring backward compatibility is maintained.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV1 {
    /// Schema version marker (always 1 for v1)
    #[serde(default = "default_v1_version")]
    pub schema_version: u32,
    /// Temperature value in the unit specified by `unit` field
    pub temp: f32,
    /// Unix timestamp (milliseconds)
    pub timestamp: u64,
    /// Unit: "C" (Celsius), "F" (Fahrenheit), or "K" (Kelvin)
    pub unit: alloc::string::String,
}

fn default_v1_version() -> u32 {
    1
}

impl TemperatureV1 {
    /// Create a new v1 temperature reading
    pub fn new(temp: f32, timestamp: u64, unit: &str) -> Self {
        Self {
            schema_version: 1,
            temp,
            timestamp,
            unit: alloc::string::String::from(unit),
        }
    }

    /// Convert this v1 reading to v2 format (always Celsius)
    pub fn to_v2(&self) -> Temperature {
        let celsius = match self.unit.as_str() {
            "F" => (self.temp - 32.0) * 5.0 / 9.0,
            "K" => self.temp - 273.15,
            _ => self.temp, // "C" or unknown defaults to Celsius
        };
        Temperature {
            schema_version: 2,
            celsius,
            timestamp: self.timestamp,
        }
    }
}

impl SchemaType for TemperatureV1 {
    const NAME: &'static str = "temperature_v1";
    const VERSION: u32 = 1;
}

#[cfg(feature = "linkable")]
impl Linkable for TemperatureV1 {
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        serde_json::from_slice(data).map_err(|e| alloc::string::ToString::to_string(&e))
    }

    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        serde_json::to_vec(self).map_err(|e| alloc::string::ToString::to_string(&e))
    }
}

// ═══════════════════════════════════════════════════════════════════
// TYPE-SAFE MIGRATION (v1 → v2)
// ═══════════════════════════════════════════════════════════════════

/// Migration step: Temperature v1 (temp + unit) → v2 (celsius only)
#[cfg(feature = "migratable")]
pub struct TemperatureV1ToV2;

#[cfg(feature = "migratable")]
impl MigrationStep for TemperatureV1ToV2 {
    type Older = TemperatureV1;
    type Newer = Temperature;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: TemperatureV1) -> Result<Temperature, MigrationError> {
        Ok(v1.to_v2())
    }

    fn down(v2: Temperature) -> Result<TemperatureV1, MigrationError> {
        Ok(TemperatureV1 {
            schema_version: 1,
            temp: v2.celsius,
            timestamp: v2.timestamp,
            unit: alloc::string::String::from("C"),
        })
    }
}

#[cfg(feature = "migratable")]
crate::migration_chain! {
    type Current = Temperature;
    version_field = "schema_version";
    steps {
        TemperatureV1ToV2: TemperatureV1 => Temperature,
    }
}

impl Observable for Temperature {
    type Signal = f32;
    const ICON: &'static str = "🌡️";
    const UNIT: &'static str = "°C";

    fn signal(&self) -> f32 {
        self.celsius
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        alloc::format!(
            "{} [{}] Temperature: {:.1}{} at {}",
            Self::ICON,
            node_id,
            self.celsius,
            Self::UNIT,
            self.timestamp
        )
    }
}

#[cfg(feature = "simulatable")]
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
            schema_version: 2,
            celsius: current,
            timestamp,
        }
    }
}

impl Settable for Temperature {
    type Value = f32;

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Temperature {
            schema_version: 2,
            celsius: value,
            timestamp,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// LINKABLE WITH MIGRATION SUPPORT
// ═══════════════════════════════════════════════════════════════════

#[cfg(all(feature = "linkable", feature = "migratable"))]
impl Linkable for Temperature {
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        use crate::MigrationChain;
        Self::migrate_from_bytes(data).map_err(|e| alloc::format!("Migration error: {}", e))
    }

    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        serde_json::to_vec(self).map_err(|e| alloc::string::ToString::to_string(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settable() {
        let temp = Temperature::set(22.5, 1704326400000);
        assert_eq!(temp.schema_version, 2);
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[test]
    fn test_schema_name() {
        assert_eq!(Temperature::NAME, "temperature");
    }

    #[test]
    fn test_schema_version() {
        assert_eq!(Temperature::VERSION, 2);
    }

    // ═══════════════════════════════════════════════════════════════════
    // v1 → v2 MIGRATION TESTS
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_v1_to_v2_celsius() {
        let v1 = TemperatureV1::new(22.5, 1704326400000, "C");
        let v2 = v1.to_v2();
        assert_eq!(v2.schema_version, 2);
        assert_eq!(v2.celsius, 22.5);
        assert_eq!(v2.timestamp, 1704326400000);
    }

    #[test]
    fn test_v1_to_v2_fahrenheit() {
        // 68°F = 20°C
        let v1 = TemperatureV1::new(68.0, 1704326400000, "F");
        let v2 = v1.to_v2();
        assert!(
            (v2.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            v2.celsius
        );
    }

    #[test]
    fn test_v1_to_v2_kelvin() {
        // 293.15K = 20°C
        let v1 = TemperatureV1::new(293.15, 1704326400000, "K");
        let v2 = v1.to_v2();
        assert!(
            (v2.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            v2.celsius
        );
    }

    #[test]
    fn test_v1_to_v2_unknown_unit_defaults_to_celsius() {
        let v1 = TemperatureV1::new(22.5, 1704326400000, "X");
        let v2 = v1.to_v2();
        assert_eq!(v2.celsius, 22.5);
    }

    // ═══════════════════════════════════════════════════════════════════
    // TYPE-SAFE MIGRATION STEP TESTS
    // ═══════════════════════════════════════════════════════════════════

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migration_step_up_celsius() {
        use crate::MigrationStep;

        let v1 = TemperatureV1::new(22.5, 1704326400000, "C");
        let v2 = TemperatureV1ToV2::up(v1).unwrap();
        assert_eq!(v2.celsius, 22.5);
        assert_eq!(v2.timestamp, 1704326400000);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migration_step_up_fahrenheit() {
        use crate::MigrationStep;

        let v1 = TemperatureV1::new(68.0, 1704326400000, "F");
        let v2 = TemperatureV1ToV2::up(v1).unwrap();
        assert!(
            (v2.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            v2.celsius
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migration_step_up_kelvin() {
        use crate::MigrationStep;

        let v1 = TemperatureV1::new(293.15, 1704326400000, "K");
        let v2 = TemperatureV1ToV2::up(v1).unwrap();
        assert!(
            (v2.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            v2.celsius
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migration_step_down() {
        use crate::MigrationStep;

        let v2 = Temperature::new(22.5, 1704326400000);
        let v1 = TemperatureV1ToV2::down(v2).unwrap();
        assert_eq!(v1.temp, 22.5);
        assert_eq!(v1.timestamp, 1704326400000);
        assert_eq!(v1.unit, "C");
        assert_eq!(v1.schema_version, 1);
    }

    // ═══════════════════════════════════════════════════════════════════
    // MIGRATION CHAIN TESTS (upgrade from bytes)
    // ═══════════════════════════════════════════════════════════════════

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migrate_from_bytes_v1() {
        use crate::MigrationChain;

        let json = r#"{"schema_version":1,"temp":22.5,"timestamp":1704326400000,"unit":"C"}"#;
        let temp = Temperature::migrate_from_bytes(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migrate_from_bytes_v2_no_migration() {
        use crate::MigrationChain;

        let json = r#"{"schema_version":2,"celsius":22.5,"timestamp":1704326400000}"#;
        let temp = Temperature::migrate_from_bytes(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migrate_from_bytes_version_too_new() {
        use crate::MigrationChain;

        let json = r#"{"schema_version":99,"celsius":22.5,"timestamp":100}"#;
        let err = Temperature::migrate_from_bytes(json.as_bytes()).unwrap_err();
        assert_eq!(
            err,
            crate::MigrationError::VersionTooNew {
                source: 99,
                current: 2
            }
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migrate_from_bytes_missing_version() {
        use crate::MigrationChain;

        let json = r#"{"celsius":22.5,"timestamp":100}"#;
        let err = Temperature::migrate_from_bytes(json.as_bytes()).unwrap_err();
        assert_eq!(err, crate::MigrationError::MissingVersion);
    }

    // ═══════════════════════════════════════════════════════════════════
    // DOWNGRADE TESTS
    // ═══════════════════════════════════════════════════════════════════

    #[cfg(feature = "migratable")]
    #[test]
    fn test_downgrade_to_v1() {
        use crate::MigrationChain;

        let temp = Temperature::new(22.5, 1704326400000);
        let v1_bytes = temp.migrate_to_version(1).unwrap();
        let v1: serde_json::Value = serde_json::from_slice(&v1_bytes).unwrap();

        assert_eq!(v1["temp"], 22.5);
        assert_eq!(v1["unit"], "C");
        assert_eq!(v1["schema_version"], 1);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_downgrade_to_current_version() {
        use crate::MigrationChain;

        let temp = Temperature::new(22.5, 1704326400000);
        let v2_bytes = temp.migrate_to_version(2).unwrap();
        let v2: Temperature = serde_json::from_slice(&v2_bytes).unwrap();
        assert_eq!(v2.celsius, 22.5);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_downgrade_version_too_old() {
        use crate::MigrationChain;

        let temp = Temperature::new(22.5, 100);
        let err = temp.migrate_to_version(0).unwrap_err();
        assert_eq!(
            err,
            crate::MigrationError::VersionTooOld {
                target: 0,
                minimum: 1
            }
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_roundtrip_v1_upgrade_downgrade() {
        use crate::MigrationChain;

        // Start with v1 JSON
        let v1_json = r#"{"schema_version":1,"temp":22.5,"timestamp":1704326400000,"unit":"C"}"#;

        // Upgrade to v2
        let v2 = Temperature::migrate_from_bytes(v1_json.as_bytes()).unwrap();
        assert_eq!(v2.celsius, 22.5);

        // Downgrade back to v1
        let v1_bytes = v2.migrate_to_version(1).unwrap();
        let v1: TemperatureV1 = serde_json::from_slice(&v1_bytes).unwrap();
        assert_eq!(v1.temp, 22.5);
        assert_eq!(v1.unit, "C");

        // Upgrade again — should round-trip
        let v2_again = Temperature::migrate_from_bytes(&v1_bytes).unwrap();
        assert_eq!(v2_again.celsius, 22.5);
    }

    // ═══════════════════════════════════════════════════════════════════
    // LINKABLE TRAIT TESTS (with auto-migration)
    // ═══════════════════════════════════════════════════════════════════

    #[cfg(all(feature = "linkable", feature = "migratable"))]
    #[test]
    fn test_from_bytes_v1_with_version_marker() {
        use crate::Linkable;

        let json = r#"{"schema_version":1,"temp":68.0,"timestamp":1704326400000,"unit":"F"}"#;
        let temp = Temperature::from_bytes(json.as_bytes()).unwrap();
        assert!(
            (temp.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C from 68°F"
        );
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(all(feature = "linkable", feature = "migratable"))]
    #[test]
    fn test_from_bytes_v2_with_version_marker() {
        use crate::Linkable;

        let json = r#"{"schema_version":2,"celsius":22.5,"timestamp":1704326400000}"#;
        let temp = Temperature::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(all(feature = "linkable", feature = "migratable"))]
    #[test]
    fn test_from_bytes_v1_celsius_unit() {
        use crate::Linkable;

        let json = r#"{"schema_version":1,"temp":22.5,"timestamp":1704326400000,"unit":"C"}"#;
        let temp = Temperature::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    #[cfg(all(feature = "linkable", feature = "migratable"))]
    #[test]
    fn test_from_bytes_via_linkable_trait() {
        use crate::Linkable;

        // v1 payload should auto-migrate
        let v1_json = r#"{"schema_version":1,"temp":68.0,"timestamp":1704326400000,"unit":"F"}"#;
        let temp = Temperature::from_bytes(v1_json.as_bytes()).unwrap();
        assert!((temp.celsius - 20.0).abs() < 0.01);

        // v2 payload should parse directly
        let v2_json = r#"{"schema_version":2,"celsius":22.5,"timestamp":1704326400000}"#;
        let temp = Temperature::from_bytes(v2_json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    #[cfg(all(feature = "linkable", feature = "migratable"))]
    #[test]
    fn test_from_bytes_missing_version_fails() {
        use crate::Linkable;

        let json = r#"{"celsius":22.5,"timestamp":1704326400000}"#;
        let result = Temperature::from_bytes(json.as_bytes());
        assert!(result.is_err());
    }

    #[cfg(feature = "linkable")]
    #[test]
    fn test_v1_serialization_includes_schema_version() {
        use crate::Linkable;

        let v1 = TemperatureV1::new(22.5, 1704326400000, "C");
        let bytes = v1.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["temp"], 22.5);
        assert_eq!(json["unit"], "C");
    }

    #[cfg(feature = "simulatable")]
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

    #[cfg(feature = "simulatable")]
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
