//! Behavioral coverage for `#[derive(Linkable)]`.
//!
//! Lives under `tests/` (a separate crate), not `#[cfg(test)] mod tests`
//! inside `src/linkable.rs`: the derive emits absolute
//! `::aimdb_data_contracts::...` paths (matching the `migration_chain!` /
//! `RecordKey` precedent), which only resolve from outside the defining crate.

#![cfg(feature = "linkable")]

use aimdb_data_contracts::{Linkable, SchemaType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Linkable)]
struct DerivedTemperature {
    celsius: f32,
    timestamp: u64,
}

impl SchemaType for DerivedTemperature {
    const NAME: &'static str = "derived_temperature";
}

#[test]
fn roundtrips_through_json() {
    let temp = DerivedTemperature {
        celsius: 22.5,
        timestamp: 1704326400000,
    };

    let bytes = temp.to_bytes().expect("serialization should succeed");
    let restored = DerivedTemperature::from_bytes(&bytes).expect("deserialization should succeed");

    assert_eq!(temp, restored);
}

#[test]
fn parses_hand_written_json() {
    let json = br#"{"celsius": 25.0, "timestamp": 1704326400000}"#;
    let temp = DerivedTemperature::from_bytes(json).expect("should parse valid JSON");

    assert_eq!(temp.celsius, 25.0);
    assert_eq!(temp.timestamp, 1704326400000);
}

#[test]
fn rejects_invalid_json() {
    let invalid = b"not valid json";
    assert!(DerivedTemperature::from_bytes(invalid).is_err());
}
