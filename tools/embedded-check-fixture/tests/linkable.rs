//! The contracts' `Linkable` codecs, as a receiving node uses them.

use aimdb_data_contracts::{link_codecs::Json, Linkable, MigrationChain};
use embedded_check_fixture::contracts::{TemperatureV3, TemperatureV4};

#[test]
fn a_v3_payload_is_migrated_with_an_explicit_version() {
    let v3 = br#"{"temp":212.0,"timestamp":7,"unit":"F"}"#;
    let t = TemperatureV4::migrate_from(3, v3, &Json).expect("v3 decodes through the chain");
    assert_eq!(t, TemperatureV4::new(100.0, 7));
}

#[test]
fn v4_round_trips() {
    let t = TemperatureV4::new(21.5, 9);
    let bytes = t.to_bytes().expect("encodes");
    assert_eq!(TemperatureV4::from_bytes(&bytes).expect("decodes"), t);
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(payload.get("schema_version").is_none());
}

#[test]
fn v3_round_trips_as_itself() {
    let t = TemperatureV3 {
        temp: 295.15,
        timestamp: 3,
        unit: "K".into(),
    };
    let bytes = t.to_bytes().expect("encodes");
    assert_eq!(TemperatureV3::from_bytes(&bytes).expect("decodes"), t);
}

#[test]
fn frozen_legacy_payloads_upgrade_without_probing_their_version_field() {
    // Deliberately contradictory ordinary data: only migrate_from's argument
    // selects the historical type.
    for (version, payload) in [
        (
            1,
            &br#"{"schema_version":999,"temp":212.0,"timestamp":7,"unit":"F"}"#[..],
        ),
        (
            2,
            &br#"{"schema_version":999,"celsius":100.0,"timestamp":7}"#[..],
        ),
    ] {
        let current = TemperatureV4::migrate_from(version, payload, &Json).unwrap();
        assert_eq!(current, TemperatureV4::new(100.0, 7));
        let mut out = [0; 256];
        let len = current.migrate_into(version, &Json, &mut out).unwrap();
        let legacy: serde_json::Value = serde_json::from_slice(&out[..len]).unwrap();
        assert_eq!(legacy["schema_version"], version);
        assert_eq!(
            TemperatureV4::migrate_from(version, &out[..len], &Json).unwrap(),
            current
        );
    }
}
