//! JSON codec for records (feature `remote`, no_std + alloc compatible).
//!
//! A record's value `T` often has to cross a type-erasure boundary into a wire
//! format. AimX (std) crosses into `serde_json::Value`; so can a no_std
//! connector or a local `record.latest()?.as_json()` call. This module
//! provides that bridge without requiring `std` — `serde_json` runs on `alloc`
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
}
