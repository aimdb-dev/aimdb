//! GPS location schema

use crate::{Observable, SchemaType, Settable};
use serde::{Deserialize, Serialize};

#[cfg(feature = "linkable")]
use crate::Linkable;

#[cfg(feature = "simulatable")]
use crate::{Simulatable, SimulationConfig};

#[cfg(feature = "ts")]
use ts_rs::TS;

/// GPS location reading
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(TS))]
#[cfg_attr(feature = "ts", ts(export))]
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

impl Observable for GpsLocation {
    /// Signal is (latitude, longitude) tuple.
    /// Ordering is lexicographic (lat first, then lon).
    type Signal = (f64, f64);

    const ICON: &'static str = "📍";
    const UNIT: &'static str = "°";

    fn signal(&self) -> Self::Signal {
        (self.latitude, self.longitude)
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        let alt_str = match self.altitude {
            Some(alt) => alloc::format!(" alt={:.1}m", alt),
            None => alloc::string::String::new(),
        };
        let acc_str = match self.accuracy {
            Some(acc) => alloc::format!(" acc=±{:.1}m", acc),
            None => alloc::string::String::new(),
        };
        alloc::format!(
            "{} [{}] GpsLocation: {:.6}{}, {:.6}{}{}{}",
            Self::ICON,
            node_id,
            self.latitude,
            Self::UNIT,
            self.longitude,
            Self::UNIT,
            alt_str,
            acc_str
        )
    }
}

#[cfg(feature = "simulatable")]
impl Simulatable for GpsLocation {
    /// Simulate GPS readings with random walk behavior around a base location.
    ///
    /// # Config params interpretation
    /// - `base`: Base latitude (default: 48.2082 - Vienna)
    /// - `variation`: Maximum wander radius in degrees (default: 0.001 ≈ 111m)
    /// - `step`: Random walk step multiplier (default: 0.2)
    /// - `trend`: Not used for GPS
    fn simulate<R: rand::Rng>(
        config: &SimulationConfig,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp: u64,
    ) -> Self {
        // Use base as latitude, and a fixed longitude offset
        let base_lat = config.params.base;
        let base_lon = 16.3738; // Vienna longitude as default
        let max_delta = config.params.variation;
        let step = config.params.step;

        // Random walk from previous position or start near base
        let (lat, lon) = match previous {
            Some(prev) => {
                let lat_delta = (rng.gen::<f64>() - 0.5) * max_delta * step;
                let lon_delta = (rng.gen::<f64>() - 0.5) * max_delta * step;
                let new_lat =
                    (prev.latitude + lat_delta).clamp(base_lat - max_delta, base_lat + max_delta);
                let new_lon =
                    (prev.longitude + lon_delta).clamp(base_lon - max_delta, base_lon + max_delta);
                (new_lat, new_lon)
            }
            None => {
                let lat = base_lat + (rng.gen::<f64>() - 0.5) * max_delta;
                let lon = base_lon + (rng.gen::<f64>() - 0.5) * max_delta;
                (lat, lon)
            }
        };

        GpsLocation {
            latitude: lat,
            longitude: lon,
            altitude: Some(200.0 + rng.gen::<f32>() * 10.0),
            accuracy: Some(5.0 + rng.gen::<f32>() * 10.0),
            timestamp,
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
        let loc = GpsLocation::set((48.2082, 16.3738, Some(200.0), Some(5.0)), 1704326400000);
        assert_eq!(loc.latitude, 48.2082);
        assert_eq!(loc.longitude, 16.3738);
        assert_eq!(loc.altitude, Some(200.0));
        assert_eq!(loc.accuracy, Some(5.0));
        assert_eq!(loc.timestamp, 1704326400000);
    }

    #[test]
    fn test_settable_without_optional() {
        let loc = GpsLocation::set((48.2082, 16.3738, None, None), 1704326400000);
        assert_eq!(loc.latitude, 48.2082);
        assert_eq!(loc.longitude, 16.3738);
        assert_eq!(loc.altitude, None);
        assert_eq!(loc.accuracy, None);
    }

    #[test]
    fn test_schema_name() {
        assert_eq!(GpsLocation::NAME, "gps_location");
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
                base: 48.2082,    // Vienna latitude
                variation: 0.001, // ~111 meters
                trend: 0.0,
                step: 0.2,
            },
        };

        let mut rng = StdRng::seed_from_u64(42);

        // Generate first sample
        let loc1 = GpsLocation::simulate(&config, None, &mut rng, 1000);
        assert!(loc1.latitude >= 48.207 && loc1.latitude <= 48.210);
        assert!(loc1.altitude.is_some());

        // Generate second sample (should be close to first due to random walk)
        let loc2 = GpsLocation::simulate(&config, Some(&loc1), &mut rng, 2000);
        let lat_diff = (loc2.latitude - loc1.latitude).abs();
        assert!(
            lat_diff < 0.0005,
            "Random walk step too large: {}",
            lat_diff
        );
    }

    #[test]
    fn test_observable() {
        let loc = GpsLocation::set((48.2082, 16.3738, Some(350.0), None), 1000);
        assert_eq!(loc.signal(), (48.2082, 16.3738));

        // Lexicographic comparison: lat first, then lon
        let loc_north = GpsLocation::set((49.0, 16.0, None, None), 1000);
        let loc_south = GpsLocation::set((47.0, 17.0, None, None), 1000);
        assert!(loc_north.signal() > loc_south.signal()); // 49 > 47

        // Test icon and unit
        assert_eq!(GpsLocation::ICON, "📍");
        assert_eq!(GpsLocation::UNIT, "°");

        // Test format_log with all fields
        let loc_full = GpsLocation::set((48.2082, 16.3738, Some(350.0), Some(5.5)), 1000);
        let log = loc_full.format_log("alpha");
        assert!(log.contains("📍"));
        assert!(log.contains("[alpha]"));
        assert!(log.contains("48.208200°"));
        assert!(log.contains("16.373800°"));
        assert!(log.contains("alt=350.0m"));
        assert!(log.contains("acc=±5.5m"));

        // Test format_log without optional fields
        let loc_minimal = GpsLocation::set((48.2082, 16.3738, None, None), 2000);
        let log_min = loc_minimal.format_log("beta");
        assert!(log_min.contains("📍"));
        assert!(log_min.contains("[beta]"));
        assert!(!log_min.contains("alt="));
        assert!(!log_min.contains("acc="));
    }
}
