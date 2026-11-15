//! Contracts as an application's contracts crate writes them: JSON on the
//! wire through `Linkable`, a migration chain for the older temperature
//! schema, and every contract locked in `aimdb.lock`.

use aimdb_data_contracts::{
    Linkable, MigrationError, MigrationStep, SchemaType, Streamable, WireFormat,
};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

// Versions 1 and 2 included an in-band version field. Removing it changes
// the wire shape, so the current schemas use versions 3 and 4. Frozen
// legacy payloads remain in the chain for deployed devices.
// The same `NAME@VERSION` always has the same wire shape; the generated test
// checks the catalog against `aimdb.lock`.
aimdb_data_contracts::catalog! {
    contracts: [legacy::TemperatureV1, legacy::TemperatureV2, TemperatureV3, TemperatureV4, Humidity, DewPoint],
    helpers: [],
}

#[cfg(feature = "sim")]
use aimdb_data_contracts::{RandomWalkParams, Simulatable};
#[cfg(feature = "sim")]
use rand::RngExt;

/// Temperature, schema v4: Celsius only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV4 {
    pub celsius: f32,
    pub timestamp: u64,
}

/// The current temperature schema.
pub type Temperature = TemperatureV4;

impl TemperatureV4 {
    pub fn new(celsius: f32, timestamp: u64) -> Self {
        Self { celsius, timestamp }
    }
}

impl SchemaType for TemperatureV4 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 4;
}

impl Streamable for TemperatureV4 {}

/// Temperature, schema v3: a value and its unit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Linkable)]
pub struct TemperatureV3 {
    pub temp: f32,
    pub timestamp: u64,
    /// "C", "F" or "K".
    pub unit: String,
}

impl SchemaType for TemperatureV3 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 3;
}

/// v3 (value and unit) to v4 (Celsius).
pub struct TemperatureV3ToV4;

impl MigrationStep for TemperatureV3ToV4 {
    type Older = TemperatureV3;
    type Newer = TemperatureV4;
    const FROM_VERSION: u32 = 3;
    const TO_VERSION: u32 = 4;

    fn up(v1: TemperatureV3) -> Result<TemperatureV4, MigrationError> {
        let celsius = match v1.unit.as_str() {
            "F" => (v1.temp - 32.0) * 5.0 / 9.0,
            "K" => v1.temp - 273.15,
            _ => v1.temp,
        };
        Ok(TemperatureV4::new(celsius, v1.timestamp))
    }

    fn down(v2: TemperatureV4) -> Result<TemperatureV3, MigrationError> {
        Ok(TemperatureV3 {
            temp: v2.celsius,
            timestamp: v2.timestamp,
            unit: String::from("C"),
        })
    }
}

aimdb_data_contracts::migration_chain! {
    type Current = TemperatureV4;
    steps {
        legacy::TemperatureV1ToV2: legacy::TemperatureV1 => legacy::TemperatureV2,
        legacy::TemperatureV2ToV3: legacy::TemperatureV2 => TemperatureV3,
        TemperatureV3ToV4: TemperatureV3 => TemperatureV4,
    }
}

/// The default bare link reads the current schema. Older payloads require an
/// explicit version passed to MigrationChain (later installed by link admission).
impl Linkable for TemperatureV4 {
    const WIRE_FORMAT: WireFormat = WireFormat::Json;

    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(data).map_err(|e| e.to_string())
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }
}

/// Relative humidity in percent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Linkable)]
pub struct Humidity {
    pub percent: f32,
    pub timestamp: u64,
}

impl SchemaType for Humidity {
    const NAME: &'static str = "humidity";
}

impl Streamable for Humidity {}

/// Dew point, derived from temperature and humidity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Linkable)]
pub struct DewPoint {
    pub celsius: f32,
    pub timestamp: u64,
}

impl DewPoint {
    /// Magnus approximation: `T_dp ≈ T - (100 - RH) / 5`.
    pub fn from_readings(t: &Temperature, h: &Humidity) -> Self {
        Self {
            celsius: t.celsius - (100.0 - h.percent) / 5.0,
            timestamp: t.timestamp.max(h.timestamp),
        }
    }
}

impl SchemaType for DewPoint {
    const NAME: &'static str = "dew_point";
}

impl Streamable for DewPoint {}

#[cfg(feature = "sim")]
fn walk<R: rand::Rng>(
    params: &RandomWalkParams,
    previous: Option<f32>,
    rng: &mut R,
    min: f32,
    max: f32,
) -> f32 {
    let base = params.base as f32;
    let variation = params.variation as f32;
    match previous {
        Some(prev) => {
            let delta = (rng.random::<f32>() - 0.5) * variation * params.step as f32;
            (prev + delta + params.trend as f32)
                .clamp(min, max)
                .clamp(base - variation, base + variation)
        }
        None => base + (rng.random::<f32>() - 0.5) * variation,
    }
}

#[cfg(feature = "sim")]
impl Simulatable for TemperatureV4 {
    type Params = RandomWalkParams;

    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self {
        let celsius = walk(params, previous.map(|p| p.celsius), rng, -273.15, f32::MAX);
        Self::new(celsius, timestamp_ms)
    }
}

#[cfg(feature = "sim")]
impl Simulatable for Humidity {
    type Params = RandomWalkParams;

    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            percent: walk(params, previous.map(|p| p.percent), rng, 0.0, 100.0),
            timestamp: timestamp_ms,
        }
    }
}

/// Frozen deployed layouts: `schema_version` is ordinary legacy payload data.
/// Dispatch uses the envelope's version, never either field's value.
mod legacy {
    use super::*;

    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TemperatureV1 {
        #[serde(default = "v1")]
        pub schema_version: u32,
        pub temp: f32,
        pub timestamp: u64,
        pub unit: String,
    }
    fn v1() -> u32 {
        1
    }
    impl SchemaType for TemperatureV1 {
        const NAME: &'static str = "temperature";
        const VERSION: u32 = 1;
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TemperatureV2 {
        #[serde(default = "v2")]
        pub schema_version: u32,
        pub celsius: f32,
        pub timestamp: u64,
    }
    fn v2() -> u32 {
        2
    }
    impl SchemaType for TemperatureV2 {
        const NAME: &'static str = "temperature";
        const VERSION: u32 = 2;
    }

    pub struct TemperatureV1ToV2;
    impl MigrationStep for TemperatureV1ToV2 {
        type Older = TemperatureV1;
        type Newer = TemperatureV2;
        const FROM_VERSION: u32 = 1;
        const TO_VERSION: u32 = 2;
        fn up(v: TemperatureV1) -> Result<TemperatureV2, MigrationError> {
            let celsius = match v.unit.as_str() {
                "F" => (v.temp - 32.0) * 5.0 / 9.0,
                "K" => v.temp - 273.15,
                _ => v.temp,
            };
            Ok(TemperatureV2 {
                schema_version: 2,
                celsius,
                timestamp: v.timestamp,
            })
        }
        fn down(v: TemperatureV2) -> Result<TemperatureV1, MigrationError> {
            Ok(TemperatureV1 {
                schema_version: 1,
                temp: v.celsius,
                timestamp: v.timestamp,
                unit: "C".into(),
            })
        }
    }

    pub struct TemperatureV2ToV3;
    impl MigrationStep for TemperatureV2ToV3 {
        type Older = TemperatureV2;
        type Newer = TemperatureV3;
        const FROM_VERSION: u32 = 2;
        const TO_VERSION: u32 = 3;
        fn up(v: TemperatureV2) -> Result<TemperatureV3, MigrationError> {
            Ok(TemperatureV3 {
                temp: v.celsius,
                timestamp: v.timestamp,
                unit: "C".into(),
            })
        }
        fn down(v: TemperatureV3) -> Result<TemperatureV2, MigrationError> {
            let v = TemperatureV3ToV4::up(v)?;
            Ok(TemperatureV2 {
                schema_version: 2,
                celsius: v.celsius,
                timestamp: v.timestamp,
            })
        }
    }
}
