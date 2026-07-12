//! Linkable registrar extension: one-line link verbs for connector wiring.
//!
//! Implementing [`Linkable`](crate::Linkable) unlocks two verbs —
//! [`LinkableRegistrarExt::linked_from`] / [`linked_to`](LinkableRegistrarExt::linked_to)
//! — that install the raw `.link_from()`/`.link_to()` builders with the codec
//! defaulted to `T::from_bytes`/`T::to_bytes`. The raw builders remain the
//! escape hatch for per-link options (QoS, topic providers/resolvers).

use aimdb_core::connector::SerializeError;
use aimdb_core::typed_api::RecordRegistrar;

use crate::Linkable;

/// Adds `.linked_from(url)` and `.linked_to(url)` to [`RecordRegistrar`] for
/// [`Linkable`] types.
pub trait LinkableRegistrarExt<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// `.link_from(url)` with the codec defaulted to `T::from_bytes`.
    fn linked_from(&mut self, url: &str) -> &mut RecordRegistrar<'a, T>;

    /// `.link_to(url)` with the codec defaulted to `T::to_bytes` and an
    /// optional bounded [`Linkable::encode_into`] fast path.
    ///
    /// `Linkable::to_bytes`'s `String` error is mapped to
    /// `SerializeError::InvalidData` — the connector layer's serializer error
    /// type carries no string detail.
    fn linked_to(&mut self, url: &str) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> LinkableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn linked_from(&mut self, url: &str) -> &mut RecordRegistrar<'a, T> {
        self.link_from(url)
            .with_deserializer(|_ctx, bytes| T::from_bytes(bytes))
            .finish()
    }

    fn linked_to(&mut self, url: &str) -> &mut RecordRegistrar<'a, T> {
        let builder = self.link_to(url).with_serializer(|_ctx, value: &T| {
            value.to_bytes().map_err(|_| SerializeError::InvalidData)
        });

        match T::ENCODE_BUFFER_CAPACITY {
            Some(capacity) => builder
                .with_serializer_into(capacity, |_ctx, value: &T, out| value.encode_into(out))
                .finish(),
            None => builder.finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

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

    #[test]
    fn default_encode_into_copies_exact_payload_and_preserves_tail() {
        let temp = TestTemp {
            celsius: 22.5,
            timestamp: 1_704_326_400_000,
        };
        let expected = temp.to_bytes().expect("serialization should succeed");
        let mut out = vec![0xA5; expected.len() + 4];

        let written = temp
            .encode_into(&mut out)
            .expect("compatibility encode should succeed");

        assert_eq!(written, expected.len());
        assert_eq!(&out[..written], expected.as_slice());
        assert_eq!(&out[written..], &[0xA5; 4]);
    }

    #[test]
    fn default_encode_into_accepts_exact_size_buffer() {
        let temp = TestTemp {
            celsius: 22.5,
            timestamp: 1_704_326_400_000,
        };
        let expected = temp.to_bytes().expect("serialization should succeed");
        let mut out = vec![0_u8; expected.len()];

        let written = temp
            .encode_into(&mut out)
            .expect("exact-size output should succeed");

        assert_eq!(written, expected.len());
        assert_eq!(out, expected);
    }

    #[test]
    fn default_encode_into_rejects_small_buffer_without_mutating_it() {
        use aimdb_core::connector::SerializeError;

        let temp = TestTemp {
            celsius: 22.5,
            timestamp: 1_704_326_400_000,
        };
        let required = temp.to_bytes().expect("serialization should succeed").len();
        let mut out = vec![0xA5; required - 1];
        let before = out.clone();

        let err = temp
            .encode_into(&mut out)
            .expect_err("undersized output must fail");

        assert_eq!(err, SerializeError::BufferTooSmall);
        assert_eq!(out, before);
    }
}
