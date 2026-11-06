//! Contracts as an application's contracts crate writes them.

use aimdb_data_contracts::{SchemaType, Streamable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "migratable")]
use aimdb_data_contracts::{MigrationError, MigrationStep};

#[cfg(feature = "sim")]
use aimdb_data_contracts::{RandomWalkParams, Simulatable};
#[cfg(feature = "sim")]
use rand::RngExt;

/// Temperature, schema v2: Celsius only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV2 {
    #[serde(default = "v2")]
    pub schema_version: u32,
    pub celsius: f32,
    pub timestamp: u64,
}

/// The current temperature schema.
pub type Temperature = TemperatureV2;

fn v2() -> u32 {
    2
}

impl TemperatureV2 {
    pub fn new(celsius: f32, timestamp: u64) -> Self {
        Self {
            schema_version: 2,
            celsius,
            timestamp,
        }
    }
}

impl SchemaType for TemperatureV2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}

impl Streamable for TemperatureV2 {}

/// Temperature, schema v1: a value and its unit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV1 {
    #[serde(default = "v1")]
    pub schema_version: u32,
    pub temp: f32,
    pub timestamp: u64,
    /// "C", "F" or "K".
    pub unit: alloc::string::String,
}

fn v1() -> u32 {
    1
}

impl SchemaType for TemperatureV1 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 1;
}

/// v1 (value and unit) to v2 (Celsius).
#[cfg(feature = "migratable")]
pub struct TemperatureV1ToV2;

#[cfg(feature = "migratable")]
impl MigrationStep for TemperatureV1ToV2 {
    type Older = TemperatureV1;
    type Newer = TemperatureV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: TemperatureV1) -> Result<TemperatureV2, MigrationError> {
        let celsius = match v1.unit.as_str() {
            "F" => (v1.temp - 32.0) * 5.0 / 9.0,
            "K" => v1.temp - 273.15,
            _ => v1.temp,
        };
        Ok(TemperatureV2::new(celsius, v1.timestamp))
    }

    fn down(v2: TemperatureV2) -> Result<TemperatureV1, MigrationError> {
        Ok(TemperatureV1 {
            schema_version: 1,
            temp: v2.celsius,
            timestamp: v2.timestamp,
            unit: alloc::string::String::from("C"),
        })
    }
}

#[cfg(feature = "migratable")]
aimdb_data_contracts::migration_chain! {
    type Current = TemperatureV2;
    version_field = "schema_version";
    steps {
        TemperatureV1ToV2: TemperatureV1 => TemperatureV2,
    }
}

/// Relative humidity in percent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Humidity {
    pub percent: f32,
    pub timestamp: u64,
}

impl SchemaType for Humidity {
    const NAME: &'static str = "humidity";
}

impl Streamable for Humidity {}

/// Dew point, derived from temperature and humidity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
impl Simulatable for TemperatureV2 {
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
