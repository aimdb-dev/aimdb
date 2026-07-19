//! Reproducible JSON/CBOR record-IR fixtures for issue #156.
//!
//! This module intentionally lives in the unpublished benchmark crate. It
//! compares representation boundaries without adding a CBOR dependency or a
//! new codec API to production AimDB crates.

use std::collections::BTreeMap;
use std::fmt;

use ciborium::Value as CborValue;
use serde::de::{DeserializeOwned, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;

const TAGGED_BYTES_KEY: &str = "$aimdb.cbor.bytes";

/// JSON compatibility policy used while transcoding a dynamic CBOR tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CborJsonPolicy {
    /// Reject CBOR-only values instead of silently changing their meaning.
    CommonSubset,
    /// Preserve byte strings through an explicit tagged JSON integer array.
    ///
    /// This is a benchmark convention, not an accepted production wire format.
    TaggedByteArray,
}

/// Errors produced by the benchmark-only representation helpers.
#[derive(Debug)]
pub enum RemoteIrError {
    /// JSON serialization or deserialization failed.
    Json(serde_json::Error),
    /// CBOR serialization, deserialization, or dynamic-value conversion failed.
    Cbor(String),
    /// The value is outside the explicitly supported CBOR/JSON common model.
    Incompatible(&'static str),
    /// An encode/decode path did not reproduce the original fixture.
    RoundTrip(&'static str),
}

impl fmt::Display for RemoteIrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Cbor(error) => write!(formatter, "CBOR error: {error}"),
            Self::Incompatible(reason) => {
                write!(formatter, "CBOR/JSON value is incompatible: {reason}")
            }
            Self::RoundTrip(path) => write!(formatter, "{path} failed to round-trip"),
        }
    }
}

impl std::error::Error for RemoteIrError {}

impl From<serde_json::Error> for RemoteIrError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// One representation path measured by both benchmark binaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteIrPath {
    /// `T -> serde_json::Value -> JSON bytes` (the current remote-access model).
    JsonTree,
    /// `T -> JSON bytes` without materializing a dynamic tree.
    JsonDirect,
    /// `T -> ciborium::Value -> JSON-compatible Value -> JSON bytes`.
    CborJsonTranscode,
    /// `T -> ciborium::Value -> CBOR bytes`.
    CborTreeNative,
    /// `T -> CBOR bytes` without materializing a dynamic tree.
    CborDirect,
}

impl RemoteIrPath {
    /// Stable order and names shared by output tables and Criterion baselines.
    pub const ALL: [Self; 5] = [
        Self::JsonTree,
        Self::JsonDirect,
        Self::CborJsonTranscode,
        Self::CborTreeNative,
        Self::CborDirect,
    ];

    /// Stable benchmark identifier.
    pub const fn name(self) -> &'static str {
        match self {
            Self::JsonTree => "json_tree",
            Self::JsonDirect => "json_direct",
            Self::CborJsonTranscode => "cbor_json_transcode",
            Self::CborTreeNative => "cbor_tree_native",
            Self::CborDirect => "cbor_direct",
        }
    }

    /// Encode one benchmark fixture through this representation path.
    pub fn encode<T>(self, value: &T) -> Result<Vec<u8>, RemoteIrError>
    where
        T: RemoteIrFixture,
    {
        match self {
            Self::JsonTree => encode_json_tree(value),
            Self::JsonDirect => encode_json_direct(value),
            Self::CborJsonTranscode => encode_cbor_json_transcode(value, T::JSON_POLICY),
            Self::CborTreeNative => encode_cbor_tree(value),
            Self::CborDirect => encode_cbor_direct(value),
        }
    }

    /// Decode one benchmark fixture through this representation path.
    pub fn decode<T>(self, bytes: &[u8]) -> Result<T, RemoteIrError>
    where
        T: RemoteIrFixture,
    {
        match self {
            Self::JsonTree => decode_json_tree(bytes),
            Self::JsonDirect => decode_json_direct(bytes),
            Self::CborJsonTranscode => decode_cbor_json_transcode(bytes, T::JSON_POLICY),
            Self::CborTreeNative => decode_cbor_tree(bytes),
            Self::CborDirect => decode_cbor_direct(bytes),
        }
    }
}

/// Shared contract for deterministic benchmark record shapes.
pub trait RemoteIrFixture:
    Clone + fmt::Debug + PartialEq + Serialize + DeserializeOwned + 'static
{
    /// Stable output and Criterion identifier.
    const NAME: &'static str;

    /// Policy needed to represent this fixture through an existing JSON edge.
    const JSON_POLICY: CborJsonPolicy = CborJsonPolicy::CommonSubset;

    /// Construct one deterministic representative value.
    fn sample() -> Self;
}

/// Numeric-heavy telemetry fixture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NumericTelemetry {
    /// Logical sensor identifier.
    pub sensor_id: u32,
    /// Monotonic record sequence.
    pub sequence: u64,
    /// Device uptime in milliseconds.
    pub uptime_ms: u64,
    /// Temperature reading.
    pub temperature_c: f64,
    /// Pressure reading.
    pub pressure_hpa: f64,
    /// Signed radio signal strength.
    pub rssi_dbm: i16,
    /// Health flag.
    pub healthy: bool,
}

impl RemoteIrFixture for NumericTelemetry {
    const NAME: &'static str = "numeric_telemetry";

    fn sample() -> Self {
        Self {
            sensor_id: 7,
            sequence: 1_000_042,
            uptime_ms: 86_400_123,
            temperature_c: 23.625,
            pressure_hpa: 1_013.25,
            rssi_dbm: -67,
            healthy: true,
        }
    }
}

/// Nested device identity used by [`NestedState`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Stable device identifier.
    pub id: String,
    /// Deployment region.
    pub region: String,
    /// Small deterministic metadata map.
    pub labels: BTreeMap<String, String>,
}

/// Nested channel state used by [`NestedState`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    /// Human-readable channel name.
    pub name: String,
    /// Current reading.
    pub value: f64,
    /// Whether the channel is enabled.
    pub enabled: bool,
}

/// Nested/string-heavy state fixture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NestedState {
    /// Device metadata.
    pub device: DeviceIdentity,
    /// Per-channel state.
    pub channels: Vec<ChannelState>,
    /// Alert thresholds.
    pub thresholds: [f64; 4],
    /// Optional operator note.
    pub note: Option<String>,
}

impl RemoteIrFixture for NestedState {
    const NAME: &'static str = "nested_state";

    fn sample() -> Self {
        let labels = BTreeMap::from([
            ("building".to_string(), "north-lab".to_string()),
            ("floor".to_string(), "3".to_string()),
            ("owner".to_string(), "controls".to_string()),
        ]);

        Self {
            device: DeviceIdentity {
                id: "edge-node-042".to_string(),
                region: "eu-central".to_string(),
                labels,
            },
            channels: vec![
                ChannelState {
                    name: "temperature".to_string(),
                    value: 23.625,
                    enabled: true,
                },
                ChannelState {
                    name: "humidity".to_string(),
                    value: 48.125,
                    enabled: true,
                },
                ChannelState {
                    name: "vibration".to_string(),
                    value: 0.03125,
                    enabled: false,
                },
            ],
            thresholds: [18.0, 27.0, 35.0, 70.0],
            note: Some("scheduled calibration window".to_string()),
        }
    }
}

/// Owned byte string that asks Serde formats to use their native byte method.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinaryBlob(pub Vec<u8>);

impl Serialize for BinaryBlob {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for BinaryBlob {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BinaryBlobVisitor;

        impl<'de> Visitor<'de> for BinaryBlobVisitor {
            type Value = BinaryBlob;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a byte string or an array of byte values")
            }

            fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BinaryBlob(value.to_vec()))
            }

            fn visit_borrowed_bytes<E>(self, value: &'de [u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_bytes(value)
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BinaryBlob(value))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut bytes = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
                while let Some(byte) = sequence.next_element::<u8>()? {
                    bytes.push(byte);
                }
                Ok(BinaryBlob(bytes))
            }
        }

        deserializer.deserialize_byte_buf(BinaryBlobVisitor)
    }
}

/// Byte-heavy fixture used to expose JSON/CBOR model differences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ByteHeavySample {
    /// Sample identifier.
    pub id: u64,
    /// Payload media type.
    pub media_type: String,
    /// Native byte payload for CBOR; JSON requires an explicit convention.
    pub payload: BinaryBlob,
    /// Small integrity marker.
    pub checksum: [u8; 16],
}

impl RemoteIrFixture for ByteHeavySample {
    const NAME: &'static str = "byte_heavy";
    const JSON_POLICY: CborJsonPolicy = CborJsonPolicy::TaggedByteArray;

    fn sample() -> Self {
        let payload = (0_u16..1_024)
            .map(|index| ((index * 31 + 7) % 256) as u8)
            .collect();

        Self {
            id: 9_001,
            media_type: "application/octet-stream".to_string(),
            payload: BinaryBlob(payload),
            checksum: [
                0x0D, 0x71, 0xA4, 0x3E, 0x81, 0x19, 0x56, 0xC2, 0x72, 0xB3, 0x06, 0xE8, 0x41, 0x9F,
                0x20, 0x5A,
            ],
        }
    }
}

/// One encoded-size row printed outside timed benchmark windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodedSize {
    /// Representation path.
    pub path: RemoteIrPath,
    /// Encoded payload length.
    pub bytes: usize,
}

/// Encode every path and return stable payload-size rows.
pub fn encoded_sizes<T>() -> Result<Vec<EncodedSize>, RemoteIrError>
where
    T: RemoteIrFixture,
{
    let value = T::sample();
    RemoteIrPath::ALL
        .into_iter()
        .map(|path| {
            path.encode(&value).map(|encoded| EncodedSize {
                path,
                bytes: encoded.len(),
            })
        })
        .collect()
}

/// Verify every representation path returns the original fixture.
pub fn verify_round_trips<T>() -> Result<(), RemoteIrError>
where
    T: RemoteIrFixture,
{
    let expected = T::sample();
    for path in RemoteIrPath::ALL {
        let encoded = path.encode(&expected)?;
        let decoded: T = path.decode(&encoded)?;
        if decoded != expected {
            return Err(RemoteIrError::RoundTrip(path.name()));
        }
    }
    Ok(())
}

fn encode_json_tree<T>(value: &T) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize,
{
    let tree = serde_json::to_value(value)?;
    Ok(serde_json::to_vec(&tree)?)
}

fn decode_json_tree<T>(bytes: &[u8]) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    let tree: JsonValue = serde_json::from_slice(bytes)?;
    // Match the current `RemoteSerialize::from_json(&Value)` implementation,
    // which clones the borrowed tree before `serde_json::from_value` consumes it.
    Ok(serde_json::from_value(tree.clone())?)
}

fn encode_json_direct<T>(value: &T) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize,
{
    Ok(serde_json::to_vec(value)?)
}

fn decode_json_direct<T>(bytes: &[u8]) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_slice(bytes)?)
}

fn encode_cbor_json_transcode<T>(
    value: &T,
    policy: CborJsonPolicy,
) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize,
{
    let cbor_tree =
        CborValue::serialized(value).map_err(|error| RemoteIrError::Cbor(error.to_string()))?;
    let json_tree = cbor_to_json(&cbor_tree, policy)?;
    Ok(serde_json::to_vec(&json_tree)?)
}

fn decode_cbor_json_transcode<T>(bytes: &[u8], policy: CborJsonPolicy) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    let json_tree: JsonValue = serde_json::from_slice(bytes)?;
    let cbor_tree = json_to_cbor(&json_tree, policy)?;
    cbor_tree
        .deserialized()
        .map_err(|error| RemoteIrError::Cbor(error.to_string()))
}

fn encode_cbor_tree<T>(value: &T) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize,
{
    let tree =
        CborValue::serialized(value).map_err(|error| RemoteIrError::Cbor(error.to_string()))?;
    encode_cbor(&tree)
}

fn decode_cbor_tree<T>(bytes: &[u8]) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    let tree: CborValue = decode_cbor(bytes)?;
    tree.deserialized()
        .map_err(|error| RemoteIrError::Cbor(error.to_string()))
}

fn encode_cbor_direct<T>(value: &T) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize,
{
    encode_cbor(value)
}

fn decode_cbor_direct<T>(bytes: &[u8]) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    decode_cbor(bytes)
}

fn encode_cbor<T>(value: &T) -> Result<Vec<u8>, RemoteIrError>
where
    T: Serialize + ?Sized,
{
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(value, &mut encoded)
        .map_err(|error| RemoteIrError::Cbor(format!("{error:?}")))?;
    Ok(encoded)
}

fn decode_cbor<T>(bytes: &[u8]) -> Result<T, RemoteIrError>
where
    T: DeserializeOwned,
{
    ciborium::de::from_reader(bytes).map_err(|error| RemoteIrError::Cbor(format!("{error:?}")))
}

/// Convert a dynamic CBOR value into the explicitly supported JSON model.
pub fn cbor_to_json(value: &CborValue, policy: CborJsonPolicy) -> Result<JsonValue, RemoteIrError> {
    match value {
        CborValue::Null => Ok(JsonValue::Null),
        CborValue::Bool(value) => Ok(JsonValue::Bool(*value)),
        CborValue::Text(value) => Ok(JsonValue::String(value.clone())),
        CborValue::Integer(value) => {
            if let Ok(value) = u64::try_from(*value) {
                Ok(JsonValue::Number(value.into()))
            } else if let Ok(value) = i64::try_from(*value) {
                Ok(JsonValue::Number(value.into()))
            } else {
                Err(RemoteIrError::Incompatible(
                    "integer is outside serde_json's i64/u64 range",
                ))
            }
        }
        CborValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or(RemoteIrError::Incompatible(
                "JSON cannot represent NaN or infinity",
            )),
        CborValue::Array(values) => values
            .iter()
            .map(|value| cbor_to_json(value, policy))
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        CborValue::Map(entries) => {
            let mut object = serde_json::Map::new();
            for (key, value) in entries {
                let CborValue::Text(key) = key else {
                    return Err(RemoteIrError::Incompatible(
                        "JSON object keys must be CBOR text values",
                    ));
                };
                if object.contains_key(key) {
                    return Err(RemoteIrError::Incompatible(
                        "duplicate CBOR map keys cannot be represented safely in JSON",
                    ));
                }
                object.insert(key.clone(), cbor_to_json(value, policy)?);
            }
            Ok(JsonValue::Object(object))
        }
        CborValue::Bytes(bytes) => match policy {
            CborJsonPolicy::CommonSubset => Err(RemoteIrError::Incompatible(
                "CBOR byte strings require an explicit JSON convention",
            )),
            CborJsonPolicy::TaggedByteArray => {
                let array = bytes
                    .iter()
                    .map(|byte| JsonValue::Number((*byte).into()))
                    .collect();
                let mut wrapper = serde_json::Map::new();
                wrapper.insert(TAGGED_BYTES_KEY.to_string(), JsonValue::Array(array));
                Ok(JsonValue::Object(wrapper))
            }
        },
        CborValue::Tag(_, _) => Err(RemoteIrError::Incompatible(
            "CBOR tags require an explicit JSON convention",
        )),
        _ => Err(RemoteIrError::Incompatible(
            "unknown future CBOR value variant",
        )),
    }
}

/// Convert a JSON value into a dynamic CBOR value under the benchmark policy.
pub fn json_to_cbor(value: &JsonValue, policy: CborJsonPolicy) -> Result<CborValue, RemoteIrError> {
    match value {
        JsonValue::Null => Ok(CborValue::Null),
        JsonValue::Bool(value) => Ok(CborValue::Bool(*value)),
        JsonValue::String(value) => Ok(CborValue::Text(value.clone())),
        JsonValue::Number(value) => {
            if let Some(value) = value.as_u64() {
                Ok(CborValue::Integer(value.into()))
            } else if let Some(value) = value.as_i64() {
                Ok(CborValue::Integer(value.into()))
            } else if let Some(value) = value.as_f64() {
                Ok(CborValue::Float(value))
            } else {
                Err(RemoteIrError::Incompatible(
                    "JSON number has no supported CBOR representation",
                ))
            }
        }
        JsonValue::Array(values) => values
            .iter()
            .map(|value| json_to_cbor(value, policy))
            .collect::<Result<Vec<_>, _>>()
            .map(CborValue::Array),
        JsonValue::Object(object) => {
            if policy == CborJsonPolicy::TaggedByteArray
                && object.len() == 1
                && object.contains_key(TAGGED_BYTES_KEY)
            {
                return decode_tagged_byte_array(&object[TAGGED_BYTES_KEY]);
            }

            object
                .iter()
                .map(|(key, value)| {
                    Ok((CborValue::Text(key.clone()), json_to_cbor(value, policy)?))
                })
                .collect::<Result<Vec<_>, RemoteIrError>>()
                .map(CborValue::Map)
        }
    }
}

fn decode_tagged_byte_array(value: &JsonValue) -> Result<CborValue, RemoteIrError> {
    let JsonValue::Array(values) = value else {
        return Err(RemoteIrError::Incompatible(
            "tagged CBOR byte wrapper must contain a JSON array",
        ));
    };

    let mut bytes = Vec::with_capacity(values.len());
    for value in values {
        let Some(byte) = value.as_u64().and_then(|value| u8::try_from(value).ok()) else {
            return Err(RemoteIrError::Incompatible(
                "tagged CBOR byte array contains a non-byte value",
            ));
        };
        bytes.push(byte);
    }
    Ok(CborValue::Bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_paths_round_trip_numeric_telemetry() {
        verify_round_trips::<NumericTelemetry>().expect("numeric fixture should round-trip");
    }

    #[test]
    fn all_paths_round_trip_nested_state() {
        verify_round_trips::<NestedState>().expect("nested fixture should round-trip");
    }

    #[test]
    fn all_paths_round_trip_byte_heavy_sample() {
        verify_round_trips::<ByteHeavySample>().expect("byte fixture should round-trip");
    }

    #[test]
    fn every_path_emits_a_non_empty_payload() {
        for size in encoded_sizes::<NumericTelemetry>().expect("size snapshot") {
            assert!(size.bytes > 0, "{} emitted no bytes", size.path.name());
        }
    }

    #[test]
    fn common_subset_rejects_native_byte_strings() {
        let error = cbor_to_json(
            &CborValue::Bytes(vec![1, 2, 3]),
            CborJsonPolicy::CommonSubset,
        )
        .expect_err("byte string should require an explicit policy");
        assert!(matches!(error, RemoteIrError::Incompatible(_)));
    }

    #[test]
    fn tagged_byte_array_preserves_dynamic_byte_type() {
        let original = CborValue::Bytes(vec![0, 1, 127, 255]);
        let json = cbor_to_json(&original, CborJsonPolicy::TaggedByteArray)
            .expect("tagged byte conversion");
        let restored =
            json_to_cbor(&json, CborJsonPolicy::TaggedByteArray).expect("tagged byte restoration");
        assert_eq!(restored, original);
    }

    #[test]
    fn non_string_map_keys_are_rejected() {
        let value = CborValue::Map(vec![(
            CborValue::Integer(1_u8.into()),
            CborValue::Text("value".to_string()),
        )]);
        let error = cbor_to_json(&value, CborJsonPolicy::CommonSubset)
            .expect_err("integer map key should be rejected");
        assert!(matches!(error, RemoteIrError::Incompatible(_)));
    }

    #[test]
    fn duplicate_map_keys_are_rejected() {
        let value = CborValue::Map(vec![
            (CborValue::Text("key".to_string()), CborValue::Null),
            (CborValue::Text("key".to_string()), CborValue::Bool(true)),
        ]);
        let error = cbor_to_json(&value, CborJsonPolicy::CommonSubset)
            .expect_err("duplicate key should be rejected");
        assert!(matches!(error, RemoteIrError::Incompatible(_)));
    }

    #[test]
    fn tags_and_non_finite_floats_are_rejected() {
        let tagged = CborValue::Tag(42, Box::new(CborValue::Null));
        let tag_error = cbor_to_json(&tagged, CborJsonPolicy::CommonSubset)
            .expect_err("tag should be rejected");
        assert!(matches!(tag_error, RemoteIrError::Incompatible(_)));

        let float_error = cbor_to_json(&CborValue::Float(f64::NAN), CborJsonPolicy::CommonSubset)
            .expect_err("NaN should be rejected");
        assert!(matches!(float_error, RemoteIrError::Incompatible(_)));
    }

    #[test]
    fn malformed_payloads_fail_every_decode_path() {
        for path in RemoteIrPath::ALL {
            let decoded = path.decode::<NumericTelemetry>(b"not a valid payload");
            assert!(decoded.is_err(), "{} accepted malformed bytes", path.name());
        }
    }
}
