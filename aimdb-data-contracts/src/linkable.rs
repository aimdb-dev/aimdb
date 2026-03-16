//! Linkable trait implementations and tests.
//!
//! This module provides the wire format support for transporting schema types
//! across connector links (MQTT, KNX, etc.).

#[cfg(test)]
mod tests {
    use crate::{Linkable, SchemaType};
    use serde::{Deserialize, Serialize};

    /// Test-only temperature struct for linkable tests.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestTemp {
        celsius: f32,
        timestamp: u64,
    }

    impl SchemaType for TestTemp {
        const NAME: &'static str = "test_temp";
    }

    impl Linkable for TestTemp {
        fn from_bytes(data: &[u8]) -> Result<Self, String> {
            serde_json::from_slice(data).map_err(|e| e.to_string())
        }

        fn to_bytes(&self) -> Result<Vec<u8>, String> {
            serde_json::to_vec(self).map_err(|e| e.to_string())
        }
    }

    /// Test-only humidity struct for linkable tests.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestHumidity {
        percent: f32,
        timestamp: u64,
    }

    impl SchemaType for TestHumidity {
        const NAME: &'static str = "test_humidity";
    }

    impl Linkable for TestHumidity {
        fn from_bytes(data: &[u8]) -> Result<Self, String> {
            serde_json::from_slice(data).map_err(|e| e.to_string())
        }

        fn to_bytes(&self) -> Result<Vec<u8>, String> {
            serde_json::to_vec(self).map_err(|e| e.to_string())
        }
    }

    /// Test-only location struct for linkable tests.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestLocation {
        latitude: f64,
        longitude: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        altitude: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accuracy: Option<f32>,
        timestamp: u64,
    }

    impl SchemaType for TestLocation {
        const NAME: &'static str = "test_location";
    }

    impl Linkable for TestLocation {
        fn from_bytes(data: &[u8]) -> Result<Self, String> {
            serde_json::from_slice(data).map_err(|e| e.to_string())
        }

        fn to_bytes(&self) -> Result<Vec<u8>, String> {
            serde_json::to_vec(self).map_err(|e| e.to_string())
        }
    }

    #[test]
    fn test_temperature_roundtrip() {
        let temp = TestTemp {
            celsius: 22.5,
            timestamp: 1704326400000,
        };

        let bytes = temp.to_bytes().expect("serialization should succeed");
        let restored = TestTemp::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(temp, restored);
    }

    #[test]
    fn test_temperature_from_json_string() {
        let json = br#"{"celsius": 25.0, "timestamp": 1704326400000}"#;
        let temp = TestTemp::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(temp.celsius, 25.0);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[test]
    fn test_temperature_from_invalid_json() {
        let invalid = b"not valid json";
        assert!(TestTemp::from_bytes(invalid).is_err());
    }

    #[test]
    fn test_temperature_from_wrong_schema() {
        let json = br#"{"wrong_field": 123}"#;
        assert!(TestTemp::from_bytes(json).is_err());
    }

    #[test]
    fn test_humidity_roundtrip() {
        let humidity = TestHumidity {
            percent: 65.0,
            timestamp: 1704326400000,
        };

        let bytes = humidity.to_bytes().expect("serialization should succeed");
        let restored = TestHumidity::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(humidity, restored);
    }

    #[test]
    fn test_humidity_from_json_string() {
        let json = br#"{"percent": 72.5, "timestamp": 1704326400000}"#;
        let humidity = TestHumidity::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(humidity.percent, 72.5);
        assert_eq!(humidity.timestamp, 1704326400000);
    }

    #[test]
    fn test_location_roundtrip() {
        let location = TestLocation {
            latitude: 48.2082,
            longitude: 16.3738,
            altitude: Some(200.0),
            accuracy: Some(5.0),
            timestamp: 1704326400000,
        };

        let bytes = location.to_bytes().expect("serialization should succeed");
        let restored = TestLocation::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(location, restored);
    }

    #[test]
    fn test_location_minimal() {
        // Location with only required fields
        let json = br#"{"latitude": 48.2082, "longitude": 16.3738, "timestamp": 1704326400000}"#;
        let location = TestLocation::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(location.latitude, 48.2082);
        assert_eq!(location.longitude, 16.3738);
        assert_eq!(location.altitude, None);
        assert_eq!(location.accuracy, None);
    }

    #[test]
    fn test_error_message_is_descriptive() {
        let invalid = b"not valid json";
        let err = TestTemp::from_bytes(invalid).unwrap_err();
        assert!(!err.is_empty(), "Error message should be descriptive");
    }
}
