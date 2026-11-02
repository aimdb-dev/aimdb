//! Links for [`Linkable`](crate::Linkable) contracts.
//!
//! [`publish`] / [`subscribe`] build a link on a connector handle with the
//! codec defaulted to `T::to_bytes` / `T::from_bytes`, for `reg.output(..)` /
//! `reg.input(..)`. The registrar verbs [`LinkableRegistrarExt::linked_from`] /
//! [`linked_to`](LinkableRegistrarExt::linked_to) do the same from a full URL,
//! and [`LinkCodecRegistrarExt`] selects a different codec per link. Every
//! path reports `T::NAME` / `T::VERSION` as the record's schema. The raw
//! `.link_to()` / `.link_from()` builders remain the escape hatch.

use aimdb_core::typed_api::RecordRegistrar;
use aimdb_core::{ConnectorHandle, InboundLink, OutboundLink};

use crate::{link_codecs, LinkCodec, LinkCodecBuilderExt, Linkable};

/// An outbound link carrying contract `T`: its codec and its schema.
fn contract_out<T, C>(link: OutboundLink<T>, codec: C) -> OutboundLink<T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
    C: LinkCodec<T>,
{
    link.with_link_codec(codec).with_schema(T::NAME, T::VERSION)
}

/// An inbound link carrying contract `T`: its codec and its schema.
fn contract_in<T, C>(link: InboundLink<T>, codec: C) -> InboundLink<T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
    C: LinkCodec<T>,
{
    link.with_link_codec(codec).with_schema(T::NAME, T::VERSION)
}

/// Publishes a `T` record to `address` on `connector`, encoded with `T`'s own
/// [`Linkable`] codec; pass it to `reg.output(..)`.
///
/// `address` needs no scheme (`"sensors/temp"`); a full URL whose scheme is
/// the connector's, as a `#[link_address]` key gives, works too. The record
/// reports `T::NAME` / `T::VERSION` as its schema. The result is an ordinary
/// [`OutboundLink`], so connector options (`with_qos`, …) and
/// [`with_link_codec`](LinkCodecBuilderExt::with_link_codec), which replaces
/// the codec, apply to it.
///
/// ```rust,ignore
/// let mqtt = builder.connect(MqttConnector::new(broker_url));
/// builder.configure::<Temperature>("temp.indoor", |reg| {
///     reg.buffer(BufferCfg::SingleLatest)
///         .output(publish(mqtt, "sensors/temp/indoor").with_qos(1));
/// });
/// ```
///
/// A record type that is not a contract does not compile:
///
/// ```compile_fail,E0277
/// # use aimdb_core::{ConnectorHandle, RecordRegistrar};
/// #[derive(Clone, Debug)]
/// struct NotAContract;
///
/// fn wire(reg: &mut RecordRegistrar<'_, NotAContract>, mqtt: ConnectorHandle) {
///     reg.output(aimdb_data_contracts::publish(mqtt, "sensors/x"));
/// }
/// ```
pub fn publish<T>(connector: ConnectorHandle, address: &str) -> OutboundLink<T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    contract_out(OutboundLink::on(connector, address), link_codecs::Default)
}

/// Subscribes a `T` record to `address` on `connector`, decoded with `T`'s
/// own [`Linkable`] codec; pass it to `reg.input(..)`.
///
/// The counterpart of [`publish`]; the same address rules and options apply.
pub fn subscribe<T>(connector: ConnectorHandle, address: &str) -> InboundLink<T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    contract_in(InboundLink::on(connector, address), link_codecs::Default)
}

/// Adds `.linked_from(url)` and `.linked_to(url)` to [`RecordRegistrar`] for
/// [`Linkable`] types.
///
/// Both also report `T::NAME`/`T::VERSION` as the record's schema
/// ([`RecordRegistrar::schema`]).
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
        self.input(contract_in(InboundLink::new(url), link_codecs::Default))
    }

    fn linked_to(&mut self, url: &str) -> &mut RecordRegistrar<'a, T> {
        self.output(contract_out(OutboundLink::new(url), link_codecs::Default))
    }
}

/// Adds per-link codec variants of [`LinkableRegistrarExt`]'s shorthand verbs.
///
/// This is a sibling trait rather than new required methods on
/// `LinkableRegistrarExt`, preserving source compatibility for downstream
/// implementations of the existing public extension trait.
pub trait LinkCodecRegistrarExt<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// `.link_from(url).with_link_codec(codec).finish()`.
    fn linked_from_with<C>(&mut self, url: &str, codec: C) -> &mut RecordRegistrar<'a, T>
    where
        C: LinkCodec<T>;

    /// `.link_to(url).with_link_codec(codec).finish()`.
    fn linked_to_with<C>(&mut self, url: &str, codec: C) -> &mut RecordRegistrar<'a, T>
    where
        C: LinkCodec<T>;
}

impl<'a, T> LinkCodecRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Linkable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn linked_from_with<C>(&mut self, url: &str, codec: C) -> &mut RecordRegistrar<'a, T>
    where
        C: LinkCodec<T>,
    {
        self.input(contract_in(InboundLink::new(url), codec))
    }

    fn linked_to_with<C>(&mut self, url: &str, codec: C) -> &mut RecordRegistrar<'a, T>
    where
        C: LinkCodec<T>,
    {
        self.output(contract_out(OutboundLink::new(url), codec))
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
