//! The contracts' `Linkable` codecs, as a receiving node uses them.

use aimdb_data_contracts::Linkable;
use embedded_check_fixture::contracts::{TemperatureV1, TemperatureV2};

#[test]
fn a_v1_payload_is_migrated_on_decode() {
    let v1 = br#"{"schema_version":1,"temp":212.0,"timestamp":7,"unit":"F"}"#;
    let t = TemperatureV2::from_bytes(v1).expect("v1 decodes through the chain");
    assert_eq!(t, TemperatureV2::new(100.0, 7));
}

#[test]
fn v2_round_trips() {
    let t = TemperatureV2::new(21.5, 9);
    let bytes = t.to_bytes().expect("encodes");
    assert_eq!(TemperatureV2::from_bytes(&bytes).expect("decodes"), t);
}

#[test]
fn v1_round_trips_as_itself() {
    let t = TemperatureV1 {
        schema_version: 1,
        temp: 295.15,
        timestamp: 3,
        unit: "K".into(),
    };
    let bytes = t.to_bytes().expect("encodes");
    assert_eq!(TemperatureV1::from_bytes(&bytes).expect("decodes"), t);
}
