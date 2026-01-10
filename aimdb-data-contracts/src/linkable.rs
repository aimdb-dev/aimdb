//! Linkable trait implementations and tests.
//!
//! This module provides the wire format support for transporting schema types
//! across connector links (MQTT, KNX, etc.).

#[cfg(test)]
mod tests {
    use crate::contracts::{GpsLocation, Humidity, Temperature};
    use crate::Linkable;

    #[test]
    fn test_temperature_roundtrip() {
        let temp = Temperature {
            schema_version: 2,
            celsius: 22.5,
            timestamp: 1704326400000,
        };

        let bytes = temp.to_bytes().expect("serialization should succeed");
        let restored = Temperature::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(temp, restored);
    }

    #[test]
    fn test_temperature_from_json_string() {
        let json = br#"{"schema_version": 2, "celsius": 25.0, "timestamp": 1704326400000}"#;
        let temp = Temperature::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(temp.celsius, 25.0);
        assert_eq!(temp.timestamp, 1704326400000);
    }

    #[test]
    fn test_temperature_from_invalid_json() {
        let invalid = b"not valid json";
        assert!(Temperature::from_bytes(invalid).is_err());
    }

    #[test]
    fn test_temperature_from_wrong_schema() {
        let json = br#"{"wrong_field": 123}"#;
        assert!(Temperature::from_bytes(json).is_err());
    }

    #[test]
    fn test_humidity_roundtrip() {
        let humidity = Humidity {
            percent: 65.0,
            timestamp: 1704326400000,
        };

        let bytes = humidity.to_bytes().expect("serialization should succeed");
        let restored = Humidity::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(humidity, restored);
    }

    #[test]
    fn test_humidity_from_json_string() {
        let json = br#"{"percent": 72.5, "timestamp": 1704326400000}"#;
        let humidity = Humidity::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(humidity.percent, 72.5);
        assert_eq!(humidity.timestamp, 1704326400000);
    }

    #[test]
    fn test_gps_location_roundtrip() {
        let location = GpsLocation {
            latitude: 48.2082,
            longitude: 16.3738,
            altitude: Some(200.0),
            accuracy: Some(5.0),
            timestamp: 1704326400000,
        };

        let bytes = location.to_bytes().expect("serialization should succeed");
        let restored = GpsLocation::from_bytes(&bytes).expect("deserialization should succeed");

        assert_eq!(location, restored);
    }

    #[test]
    fn test_gps_location_minimal() {
        // GPS location with only required fields
        let json = br#"{"latitude": 48.2082, "longitude": 16.3738, "timestamp": 1704326400000}"#;
        let location = GpsLocation::from_bytes(json).expect("should parse valid JSON");

        assert_eq!(location.latitude, 48.2082);
        assert_eq!(location.longitude, 16.3738);
        assert_eq!(location.altitude, None);
        assert_eq!(location.accuracy, None);
    }

    #[test]
    fn test_error_message_is_descriptive() {
        let invalid = b"not valid json";
        let err = Temperature::from_bytes(invalid).unwrap_err();
        assert!(!err.is_empty(), "Error message should be descriptive");
    }
}
