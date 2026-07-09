//! GPS location schema

extern crate alloc;

use aimdb_data_contracts::{Observable, SchemaType, Settable, Streamable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use aimdb_data_contracts::Linkable;

#[cfg(feature = "simulatable")]
use aimdb_data_contracts::{RandomWalkParams, Simulatable};
#[cfg(feature = "simulatable")]
use rand::RngExt;

/// GPS location reading
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GpsLocation {
    /// Latitude in decimal degrees (-90 to 90)
    pub latitude: f64,
    /// Longitude in decimal degrees (-180 to 180)
    pub longitude: f64,
    /// Altitude in meters above sea level (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f32>,
    /// Horizontal accuracy in meters (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f32>,
    /// Unix timestamp (milliseconds) when reading was taken
    pub timestamp: u64,
}

impl SchemaType for GpsLocation {
    const NAME: &'static str = "gps_location";
}

impl Streamable for GpsLocation {}

impl Observable for GpsLocation {
    /// Signal is (latitude, longitude) tuple.
    /// Ordering is lexicographic (lat first, then lon).
    type Signal = (f64, f64);

    const UNIT: &'static str = "°";

    fn signal(&self) -> Self::Signal {
        (self.latitude, self.longitude)
    }
}

#[cfg(feature = "simulatable")]
impl Simulatable for GpsLocation {
    type Params = RandomWalkParams;

    /// Simulate GPS readings with random walk behavior around a base location.
    ///
    /// # Params interpretation
    /// - `base`: Base latitude (default: 48.2082 - Vienna)
    /// - `variation`: Maximum wander radius in degrees (default: 0.001 ≈ 111m)
    /// - `step`: Random walk step multiplier (default: 0.2)
    /// - `trend`: Not used for GPS
    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self {
        // Use base as latitude, and a fixed longitude offset
        let base_lat = params.base;
        let base_lon = 16.3738; // Vienna longitude as default
        let max_delta = params.variation;
        let step = params.step;

        // Random walk from previous position or start near base
        let (lat, lon) = match previous {
            Some(prev) => {
                let lat_delta = (rng.random::<f64>() - 0.5) * max_delta * step;
                let lon_delta = (rng.random::<f64>() - 0.5) * max_delta * step;
                let new_lat =
                    (prev.latitude + lat_delta).clamp(base_lat - max_delta, base_lat + max_delta);
                let new_lon =
                    (prev.longitude + lon_delta).clamp(base_lon - max_delta, base_lon + max_delta);
                (new_lat, new_lon)
            }
            None => {
                let lat = base_lat + (rng.random::<f64>() - 0.5) * max_delta;
                let lon = base_lon + (rng.random::<f64>() - 0.5) * max_delta;
                (lat, lon)
            }
        };

        GpsLocation {
            latitude: lat,
            longitude: lon,
            altitude: Some(200.0 + rng.random::<f32>() * 10.0),
            accuracy: Some(5.0 + rng.random::<f32>() * 10.0),
            timestamp: timestamp_ms,
        }
    }
}

impl Settable for GpsLocation {
    /// (latitude, longitude, altitude, accuracy)
    type Value = (f64, f64, Option<f32>, Option<f32>);

    fn set(value: Self::Value, timestamp: u64) -> Self {
        GpsLocation {
            latitude: value.0,
            longitude: value.1,
            altitude: value.2,
            accuracy: value.3,
            timestamp,
        }
    }
}

#[cfg(feature = "linkable")]
impl Linkable for GpsLocation {
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        serde_json::from_slice(data).map_err(|e| alloc::string::ToString::to_string(&e))
    }

    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        serde_json::to_vec(self).map_err(|e| alloc::string::ToString::to_string(&e))
    }
}
