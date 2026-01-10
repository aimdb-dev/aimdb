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
//! The `from_bytes_versioned()` function reads the `schema_version` from the payload
//! and migrates automatically, allowing nodes and hubs to be updated independently.

extern crate alloc;

use crate::{Observable, SchemaType, Settable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use crate::Linkable;

#[cfg(feature = "simulatable")]
use crate::{Simulatable, SimulationConfig};

#[cfg(feature = "migratable")]
use crate::{Migratable, MigrationError};

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
// MIGRATABLE IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════

#[cfg(feature = "migratable")]
impl Migratable for Temperature {
    /// Migrate raw JSON from v1 to v2 format.
    ///
    /// # v1 → v2 Migration
    /// - Rename: `temp` → `celsius`
    /// - Convert: Apply unit conversion if unit is "F" or "K"
    /// - Remove: Drop the `unit` field
    ///
    /// Delegates to `TemperatureV1::to_v2()` to keep conversion logic DRY.
    fn migrate(raw: &mut serde_json::Value, from_version: u32) -> Result<(), MigrationError> {
        if from_version < 2 {
            // Parse as v1, convert via to_v2(), then serialize back
            let v1: TemperatureV1 = serde_json::from_value(raw.clone())
                .map_err(|_| MigrationError::MissingField("temp or unit"))?;
            let v2 = v1.to_v2();
            *raw = serde_json::to_value(v2)
                .map_err(|_| MigrationError::Custom("failed to serialize migrated value"))?;
        }

        Ok(())
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
// VERSIONED DESERIALIZATION
// ═══════════════════════════════════════════════════════════════════

#[cfg(feature = "linkable")]
impl Temperature {
    /// Deserialize from bytes with automatic migration based on `schema_version` field.
    ///
    /// This function enables **decoupled deployment**: nodes and hubs can be
    /// updated independently because the hub reads the schema version from the payload.
    ///
    /// Delegates to `Migratable::deserialize_versioned` for the actual migration.
    ///
    /// # Example
    /// ```ignore
    /// let v1 = r#"{"schema_version":1,"temp":68.0,"timestamp":123,"unit":"F"}"#;
    /// let v2 = r#"{"schema_version":2,"celsius":20.0,"timestamp":123}"#;
    /// ```
    #[cfg(feature = "migratable")]
    pub fn from_bytes_versioned(data: &[u8]) -> Result<Self, alloc::string::String> {
        use crate::Migratable;

        let mut value: serde_json::Value =
            serde_json::from_slice(data).map_err(|e| alloc::format!("JSON parse error: {}", e))?;

        let version = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| alloc::string::String::from("Missing schema_version field"))?
            as u32;

        Self::deserialize_versioned(&mut value, version)
            .map_err(|e| alloc::format!("Migration error: {:?}", e))
    }
}

#[cfg(all(feature = "linkable", feature = "migratable"))]
impl Linkable for Temperature {
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        // Use versioned deserializer for automatic migration
        Self::from_bytes_versioned(data)
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

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migratable_trait_celsius() {
        use crate::Migratable;

        let mut raw = serde_json::json!({
            "temp": 22.5,
            "timestamp": 1704326400000_u64,
            "unit": "C"
        });

        Temperature::migrate(&mut raw, 1).unwrap();

        assert_eq!(raw["celsius"], 22.5);
        assert!(raw.get("temp").is_none(), "temp field should be removed");
        assert!(raw.get("unit").is_none(), "unit field should be removed");
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migratable_trait_fahrenheit() {
        use crate::Migratable;

        let mut raw = serde_json::json!({
            "temp": 68.0,
            "timestamp": 1704326400000_u64,
            "unit": "F"
        });

        Temperature::migrate(&mut raw, 1).unwrap();

        let celsius = raw["celsius"].as_f64().unwrap();
        assert!(
            (celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            celsius
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_migratable_trait_kelvin() {
        use crate::Migratable;

        let mut raw = serde_json::json!({
            "temp": 293.15,
            "timestamp": 1704326400000_u64,
            "unit": "K"
        });

        Temperature::migrate(&mut raw, 1).unwrap();

        let celsius = raw["celsius"].as_f64().unwrap();
        assert!(
            (celsius - 20.0).abs() < 0.01,
            "Expected ~20°C, got {}",
            celsius
        );
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_deserialize_versioned_v1() {
        use crate::Migratable;

        let mut raw = serde_json::json!({
            "temp": 22.5,
            "timestamp": 1704326400000_u64,
            "unit": "C"
        });

        let temp: Temperature = Temperature::deserialize_versioned(&mut raw, 1).unwrap();
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(feature = "migratable")]
    #[test]
    fn test_deserialize_versioned_v2_no_migration() {
        use crate::Migratable;

        let mut raw = serde_json::json!({
            "celsius": 22.5,
            "timestamp": 1704326400000_u64
        });

        let temp: Temperature = Temperature::deserialize_versioned(&mut raw, 2).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    // ═══════════════════════════════════════════════════════════════════
    // VERSIONED DESERIALIZATION TESTS (auto-detect version)
    // ═══════════════════════════════════════════════════════════════════

    #[cfg(feature = "linkable")]
    #[test]
    fn test_from_bytes_v1_with_version_marker() {
        let json = r#"{"schema_version":1,"temp":68.0,"timestamp":1704326400000,"unit":"F"}"#;
        let temp = Temperature::from_bytes_versioned(json.as_bytes()).unwrap();
        assert!(
            (temp.celsius - 20.0).abs() < 0.01,
            "Expected ~20°C from 68°F"
        );
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(feature = "linkable")]
    #[test]
    fn test_from_bytes_v2_with_version_marker() {
        let json = r#"{"schema_version":2,"celsius":22.5,"timestamp":1704326400000}"#;
        let temp = Temperature::from_bytes_versioned(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[cfg(feature = "linkable")]
    #[test]
    fn test_from_bytes_v1_celsius_unit() {
        let json = r#"{"schema_version":1,"temp":22.5,"timestamp":1704326400000,"unit":"C"}"#;
        let temp = Temperature::from_bytes_versioned(json.as_bytes()).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    #[cfg(feature = "linkable")]
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

    #[cfg(feature = "linkable")]
    #[test]
    fn test_from_bytes_missing_version_fails() {
        // Payloads without schema_version should fail
        let json = r#"{"celsius":22.5,"timestamp":1704326400000}"#;
        let result = Temperature::from_bytes_versioned(json.as_bytes());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("schema_version"));
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
