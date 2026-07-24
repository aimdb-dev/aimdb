//! JSON codec for records (feature `remote`, no_std + alloc compatible).
//!
//! A record's value `T` often has to cross a type-erasure boundary into a wire
//! format. AimX uses owned JSON bytes on its outbound record hot paths and
//! `serde_json::Value` where a tree is still required; no_std connectors and
//! local `record.latest()?.as_json()` calls use the same bridge. This module
//! provides both forms without requiring `std` — `serde_json` runs on `alloc`
//! alone, so embedded targets can opt in via the `remote` feature.
//!
//! Two layers:
//!
//! - [`RemoteSerialize`] — the capability trait. Blanket-implemented for every
//!   `serde` type, so any `T: Serialize + DeserializeOwned` gets a JSON codec
//!   for free. This is the AimX/connector analogue of the data-contract traits
//!   (`Streamable`, `Linkable`). It lives in `aimdb-core` rather than
//!   `aimdb-data-contracts` because that crate depends on `aimdb-core`, not the
//!   reverse — bounding a core method on `Streamable` would be a dependency
//!   cycle. Every `Streamable` type satisfies `RemoteSerialize` automatically.
//!
//! - [`JsonCodec`] — the object-safe, type-erased storage form, with the
//!   zero-sized [`SerdeJsonCodec`] implementation. A record stores
//!   `Option<Arc<dyn JsonCodec<T>>>`; the AimX read/write/subscribe paths and
//!   `RecordValue::as_json` route through it. This mirrors the connector
//!   layer's fused `SerializedSource` / `IngestFn` callbacks.

use alloc::vec::Vec;
use serde::{de::DeserializeOwned, Serialize};

/// A record type that can be encoded to / decoded from the JSON wire format.
///
/// Blanket-implemented for every `T: Serialize + DeserializeOwned`, so opting a
/// record into JSON via `with_remote_access()` requires no extra boilerplate.
/// Implement `Serialize` + `Deserialize` (e.g. via `derive`) and the type is
/// codec-ready.
pub trait RemoteSerialize: Sized {
    /// Serialize this value to a JSON value, or `None` on failure.
    fn to_json(&self) -> Option<serde_json::Value>;

    /// Deserialize a JSON value into `Self`, or `None` on schema mismatch.
    fn from_json(value: &serde_json::Value) -> Option<Self>;

    /// Serialize this value to owned JSON bytes.
    ///
    /// The default preserves compatibility with manual implementations by
    /// routing through [`to_json`](Self::to_json). The blanket Serde
    /// implementation overrides it with direct `T -> Vec<u8>` serialization,
    /// avoiding an intermediate [`serde_json::Value`] tree.
    fn to_json_vec(&self) -> Option<Vec<u8>> {
        serde_json::to_vec(&self.to_json()?).ok()
    }

    /// Deserialize one complete JSON value from `bytes`.
    ///
    /// The default preserves compatibility with manual implementations by
    /// parsing a [`serde_json::Value`] and calling
    /// [`from_json`](Self::from_json). The blanket Serde implementation
    /// overrides it with direct slice deserialization.
    fn from_json_slice(bytes: &[u8]) -> Option<Self> {
        let value = serde_json::from_slice(bytes).ok()?;
        Self::from_json(&value)
    }
}

impl<T> RemoteSerialize for T
where
    T: Serialize + DeserializeOwned,
{
    fn to_json(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }

    fn to_json_vec(&self) -> Option<Vec<u8>> {
        serde_json::to_vec(self).ok()
    }

    fn from_json_slice(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Type-erased JSON codec for one record type.
///
/// Stored as `Arc<dyn JsonCodec<T>>` inside `TypedRecord<T, R>`, where the
/// blanket `AnyRecord` impl cannot carry a `T: RemoteSerialize` bound (it must
/// also cover non-serializable record types). `T` is fixed per record, so the
/// trait is object-safe.
pub trait JsonCodec<T>: Send + Sync {
    /// Encode a typed value to JSON, or `None` on failure.
    fn encode(&self, value: &T) -> Option<serde_json::Value>;

    /// Decode a JSON value into `T`, or `None` on schema mismatch.
    fn decode(&self, value: &serde_json::Value) -> Option<T>;

    /// Encode a typed value into owned JSON bytes.
    ///
    /// Defaulted through [`encode`](Self::encode) so existing custom codecs
    /// remain source-compatible and retain their exact JSON-tree semantics.
    fn encode_to_vec(&self, value: &T) -> Option<Vec<u8>> {
        serde_json::to_vec(&self.encode(value)?).ok()
    }

    /// Decode one complete JSON value from a byte slice.
    ///
    /// Defaulted through [`decode`](Self::decode) so existing custom codecs
    /// remain source-compatible.
    fn decode_slice(&self, bytes: &[u8]) -> Option<T> {
        let value = serde_json::from_slice(bytes).ok()?;
        self.decode(&value)
    }
}

/// Zero-sized serde-backed [`JsonCodec`].
///
/// Constructed only under a `T: RemoteSerialize` bound (see
/// `TypedRecord::with_remote_access`), so the erased `JsonCodec<T>` it yields is
/// always valid.
pub struct SerdeJsonCodec;

impl<T: RemoteSerialize> JsonCodec<T> for SerdeJsonCodec {
    fn encode(&self, value: &T) -> Option<serde_json::Value> {
        value.to_json()
    }

    fn decode(&self, value: &serde_json::Value) -> Option<T> {
        T::from_json(value)
    }

    fn encode_to_vec(&self, value: &T) -> Option<Vec<u8>> {
        value.to_json_vec()
    }

    fn decode_slice(&self, bytes: &[u8]) -> Option<T> {
        T::from_json_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Fixture {
        name: String,
        optional: Option<u32>,
        bytes: Vec<u8>,
    }

    struct TreeOnlyCodec;

    impl JsonCodec<Fixture> for TreeOnlyCodec {
        fn encode(&self, value: &Fixture) -> Option<serde_json::Value> {
            serde_json::to_value(value).ok()
        }

        fn decode(&self, value: &serde_json::Value) -> Option<Fixture> {
            serde_json::from_value(value.clone()).ok()
        }
    }

    #[derive(Debug, PartialEq)]
    struct ManualRemote(u64);

    // Deliberately implements only the original required methods. This type
    // does not implement Serde, proving the new byte methods are additive.
    impl RemoteSerialize for ManualRemote {
        fn to_json(&self) -> Option<serde_json::Value> {
            Some(json!({ "value": self.0 }))
        }

        fn from_json(value: &serde_json::Value) -> Option<Self> {
            Some(Self(value.get("value")?.as_u64()?))
        }
    }

    fn fixture() -> Fixture {
        Fixture {
            name: "cảm biến \"ngoài trời\"".into(),
            optional: None,
            bytes: vec![0, 1, 127, 255],
        }
    }

    #[test]
    fn tree_only_codec_gets_default_byte_compatibility() {
        let value = fixture();
        let bytes = TreeOnlyCodec.encode_to_vec(&value).unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            TreeOnlyCodec.encode(&value).unwrap()
        );
        assert_eq!(TreeOnlyCodec.decode_slice(&bytes), Some(value));
    }

    #[test]
    fn manual_remote_serialize_gets_default_byte_compatibility() {
        let value = ManualRemote(42);
        let bytes =
            <SerdeJsonCodec as JsonCodec<ManualRemote>>::encode_to_vec(&SerdeJsonCodec, &value)
                .unwrap();

        assert_eq!(bytes, br#"{"value":42}"#);
        assert_eq!(
            <SerdeJsonCodec as JsonCodec<ManualRemote>>::decode_slice(&SerdeJsonCodec, &bytes),
            Some(value)
        );
    }

    #[test]
    fn serde_codec_byte_path_matches_json_value_semantics() {
        let value = fixture();
        let bytes =
            <SerdeJsonCodec as JsonCodec<Fixture>>::encode_to_vec(&SerdeJsonCodec, &value).unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            <SerdeJsonCodec as JsonCodec<Fixture>>::encode(&SerdeJsonCodec, &value).unwrap()
        );
        assert_eq!(
            <SerdeJsonCodec as JsonCodec<Fixture>>::decode_slice(&SerdeJsonCodec, &bytes),
            Some(value)
        );
    }

    #[test]
    fn serde_codec_rejects_malformed_trailing_and_mismatched_bytes() {
        for invalid in [
            br#"{"name":"truncated""#.as_slice(),
            br#"{"name":"ok","optional":null,"bytes":[]} trailing"#.as_slice(),
            br#"{"name":7,"optional":null,"bytes":[]}"#.as_slice(),
        ] {
            assert!(
                <SerdeJsonCodec as JsonCodec<Fixture>>::decode_slice(&SerdeJsonCodec, invalid)
                    .is_none()
            );
        }
    }
}
