//! Round-trip behavioral coverage for `migration_chain!` at 1- through
//! 5-step chain arities, plus error paths. This is the regression pin the
//! proc-macro implementation must reproduce unchanged as chain length grows.
//!
//! Lives under `tests/` (a separate crate) rather than `#[cfg(test)] mod
//! tests` inside `src/migratable.rs`: the macro emits absolute
//! `::aimdb_data_contracts::...` paths (matching the `RecordKey` /
//! `aimdb_core` precedent in `aimdb-derive`), which only resolve from
//! outside the defining crate.

#![cfg(feature = "migratable")]

use aimdb_data_contracts::{
    migration_chain, MigrationChain, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

struct Json;
impl aimdb_data_contracts::PayloadDecoder for Json {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        serde_json::from_slice(bytes)
            .map(|value| (value, bytes.len()))
            .map_err(|e| e.to_string())
    }
}
impl aimdb_data_contracts::PayloadEncoder for Json {
    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, aimdb_core::connector::SerializeError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)?;
        out.get_mut(..bytes.len())
            .ok_or(aimdb_core::connector::SerializeError::BufferTooSmall)?
            .copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}
fn json_for<T: MigrationChain>(value: &T, version: u32) -> Result<Vec<u8>, MigrationError> {
    let mut out = vec![0; 2048];
    let n = value.migrate_into(version, &Json, &mut out)?;
    out.truncate(n);
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════
// 1-step chain: Widget v1 -> v2
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WidgetV1 {
    count: u32,
}
impl aimdb_data_contracts::ContractShape for WidgetV1 {}
impl SchemaType for WidgetV1 {
    const NAME: &'static str = "widget";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WidgetV2 {
    count: u64,
}
impl aimdb_data_contracts::ContractShape for WidgetV2 {}
impl SchemaType for WidgetV2 {
    const NAME: &'static str = "widget";
    const VERSION: u32 = 2;
}

struct WidgetV1ToV2;
impl MigrationStep for WidgetV1ToV2 {
    type Older = WidgetV1;
    type Newer = WidgetV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: WidgetV1) -> Result<WidgetV2, MigrationError> {
        Ok(WidgetV2 {
            count: u64::from(v1.count),
        })
    }
    fn down(v2: WidgetV2) -> Result<WidgetV1, MigrationError> {
        Ok(WidgetV1 {
            count: v2.count as u32,
        })
    }
}

migration_chain! {
    type Current = WidgetV2;
    steps {
        WidgetV1ToV2: WidgetV1 => WidgetV2,
    }
}

#[test]
fn widget_upgrades_from_v1() {
    let v1 = WidgetV1 { count: 7 };
    let bytes = serde_json::to_vec(&v1).unwrap();
    let current = WidgetV2::migrate_from(1, &bytes, &Json).unwrap();
    assert_eq!(current, WidgetV2 { count: 7 });
}

#[test]
fn widget_parses_current_version_directly() {
    let v2 = WidgetV2 { count: 42 };
    let bytes = serde_json::to_vec(&v2).unwrap();
    let current = WidgetV2::migrate_from(2, &bytes, &Json).unwrap();
    assert_eq!(current, v2);
}

#[test]
fn widget_downgrades_to_v1() {
    let v2 = WidgetV2 { count: 9 };
    let bytes = json_for(&v2, 1).unwrap();
    let v1: WidgetV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v1, WidgetV1 { count: 9 });
}

#[test]
fn widget_serializes_current_version_directly() {
    let v2 = WidgetV2 { count: 5 };
    let bytes = json_for(&v2, 2).unwrap();
    let round_tripped: WidgetV2 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(round_tripped, v2);
}

#[test]
fn explicit_version_ignores_an_unknown_legacy_json_field() {
    let value = WidgetV2 { count: 42 };
    let bytes = br#"{"schema_version":999,"count":42}"#;
    assert_eq!(WidgetV2::migrate_from(2, bytes, &Json).unwrap(), value);
}

// ═══════════════════════════════════════════════════════════════════
// 2-step chain: Gadget v1 -> v2 -> v3
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV1 {
    watts: u32,
}
impl aimdb_data_contracts::ContractShape for GadgetV1 {}
impl SchemaType for GadgetV1 {
    const NAME: &'static str = "gadget";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV2 {
    milliwatts: u32,
}
impl aimdb_data_contracts::ContractShape for GadgetV2 {}
impl SchemaType for GadgetV2 {
    const NAME: &'static str = "gadget";
    const VERSION: u32 = 2;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV3 {
    milliwatts: u64,
}
impl aimdb_data_contracts::ContractShape for GadgetV3 {}
impl SchemaType for GadgetV3 {
    const NAME: &'static str = "gadget";
    const VERSION: u32 = 3;
}

struct GadgetV1ToV2;
impl MigrationStep for GadgetV1ToV2 {
    type Older = GadgetV1;
    type Newer = GadgetV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: GadgetV1) -> Result<GadgetV2, MigrationError> {
        Ok(GadgetV2 {
            milliwatts: v1.watts * 1000,
        })
    }
    fn down(v2: GadgetV2) -> Result<GadgetV1, MigrationError> {
        Ok(GadgetV1 {
            watts: v2.milliwatts / 1000,
        })
    }
}

struct GadgetV2ToV3;
impl MigrationStep for GadgetV2ToV3 {
    type Older = GadgetV2;
    type Newer = GadgetV3;
    const FROM_VERSION: u32 = 2;
    const TO_VERSION: u32 = 3;

    fn up(v2: GadgetV2) -> Result<GadgetV3, MigrationError> {
        Ok(GadgetV3 {
            milliwatts: u64::from(v2.milliwatts),
        })
    }
    fn down(v3: GadgetV3) -> Result<GadgetV2, MigrationError> {
        Ok(GadgetV2 {
            milliwatts: v3.milliwatts as u32,
        })
    }
}

migration_chain! {
    type Current = GadgetV3;
    steps {
        GadgetV1ToV2: GadgetV1 => GadgetV2,
        GadgetV2ToV3: GadgetV2 => GadgetV3,
    }
}

#[test]
fn gadget_upgrades_from_v1() {
    let v1 = GadgetV1 { watts: 2 };
    let bytes = serde_json::to_vec(&v1).unwrap();
    let current = GadgetV3::migrate_from(1, &bytes, &Json).unwrap();
    assert_eq!(current, GadgetV3 { milliwatts: 2000 });
}

#[test]
fn gadget_upgrades_from_v2() {
    let v2 = GadgetV2 { milliwatts: 1500 };
    let bytes = serde_json::to_vec(&v2).unwrap();
    let current = GadgetV3::migrate_from(2, &bytes, &Json).unwrap();
    assert_eq!(current, GadgetV3 { milliwatts: 1500 });
}

#[test]
fn gadget_downgrades_to_v1() {
    let v3 = GadgetV3 { milliwatts: 4000 };
    let bytes = json_for(&v3, 1).unwrap();
    let v1: GadgetV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v1, GadgetV1 { watts: 4 });
}

#[test]
fn gadget_downgrades_to_v2() {
    let v3 = GadgetV3 { milliwatts: 4000 };
    let bytes = json_for(&v3, 2).unwrap();
    let v2: GadgetV2 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v2, GadgetV2 { milliwatts: 4000 });
}

// ═══════════════════════════════════════════════════════════════════
// 3-step chain: Gizmo v1 -> v2 -> v3 -> v4
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV1 {
    label: String,
}
impl aimdb_data_contracts::ContractShape for GizmoV1 {}
impl SchemaType for GizmoV1 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV2 {
    label: String,
    revision: u32,
}
impl aimdb_data_contracts::ContractShape for GizmoV2 {}
impl SchemaType for GizmoV2 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 2;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV3 {
    label: String,
    revision: u32,
    active: bool,
}
impl aimdb_data_contracts::ContractShape for GizmoV3 {}
impl SchemaType for GizmoV3 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 3;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV4 {
    label: String,
    revision: u64,
    active: bool,
}
impl aimdb_data_contracts::ContractShape for GizmoV4 {}
impl SchemaType for GizmoV4 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 4;
}

struct GizmoV1ToV2;
impl MigrationStep for GizmoV1ToV2 {
    type Older = GizmoV1;
    type Newer = GizmoV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: GizmoV1) -> Result<GizmoV2, MigrationError> {
        Ok(GizmoV2 {
            label: v1.label,
            revision: 0,
        })
    }
    fn down(v2: GizmoV2) -> Result<GizmoV1, MigrationError> {
        Ok(GizmoV1 { label: v2.label })
    }
}

struct GizmoV2ToV3;
impl MigrationStep for GizmoV2ToV3 {
    type Older = GizmoV2;
    type Newer = GizmoV3;
    const FROM_VERSION: u32 = 2;
    const TO_VERSION: u32 = 3;

    fn up(v2: GizmoV2) -> Result<GizmoV3, MigrationError> {
        Ok(GizmoV3 {
            label: v2.label,
            revision: v2.revision,
            active: true,
        })
    }
    fn down(v3: GizmoV3) -> Result<GizmoV2, MigrationError> {
        Ok(GizmoV2 {
            label: v3.label,
            revision: v3.revision,
        })
    }
}

struct GizmoV3ToV4;
impl MigrationStep for GizmoV3ToV4 {
    type Older = GizmoV3;
    type Newer = GizmoV4;
    const FROM_VERSION: u32 = 3;
    const TO_VERSION: u32 = 4;

    fn up(v3: GizmoV3) -> Result<GizmoV4, MigrationError> {
        Ok(GizmoV4 {
            label: v3.label,
            revision: u64::from(v3.revision),
            active: v3.active,
        })
    }
    fn down(v4: GizmoV4) -> Result<GizmoV3, MigrationError> {
        Ok(GizmoV3 {
            label: v4.label,
            revision: v4.revision as u32,
            active: v4.active,
        })
    }
}

migration_chain! {
    type Current = GizmoV4;
    steps {
        GizmoV1ToV2: GizmoV1 => GizmoV2,
        GizmoV2ToV3: GizmoV2 => GizmoV3,
        GizmoV3ToV4: GizmoV3 => GizmoV4,
    }
}

#[test]
fn gizmo_upgrades_from_every_historical_version() {
    let v1 = GizmoV1 { label: "a".into() };
    let from_v1 = GizmoV4::migrate_from(1, &serde_json::to_vec(&v1).unwrap(), &Json).unwrap();
    assert_eq!(
        from_v1,
        GizmoV4 {
            label: "a".into(),
            revision: 0,
            active: true
        }
    );

    let v2 = GizmoV2 {
        label: "b".into(),
        revision: 5,
    };
    let from_v2 = GizmoV4::migrate_from(2, &serde_json::to_vec(&v2).unwrap(), &Json).unwrap();
    assert_eq!(
        from_v2,
        GizmoV4 {
            label: "b".into(),
            revision: 5,
            active: true
        }
    );

    let v3 = GizmoV3 {
        label: "c".into(),
        revision: 9,
        active: false,
    };
    let from_v3 = GizmoV4::migrate_from(3, &serde_json::to_vec(&v3).unwrap(), &Json).unwrap();
    assert_eq!(
        from_v3,
        GizmoV4 {
            label: "c".into(),
            revision: 9,
            active: false
        }
    );

    let v4 = GizmoV4 {
        label: "d".into(),
        revision: 12,
        active: true,
    };
    let from_v4 = GizmoV4::migrate_from(4, &serde_json::to_vec(&v4).unwrap(), &Json).unwrap();
    assert_eq!(from_v4, v4);
}

#[test]
fn gizmo_downgrades_to_every_target_version() {
    let current = GizmoV4 {
        label: "z".into(),
        revision: 3,
        active: true,
    };

    let to_v1: GizmoV1 = serde_json::from_slice(&json_for(&current, 1).unwrap()).unwrap();
    assert_eq!(to_v1, GizmoV1 { label: "z".into() });

    let to_v2: GizmoV2 = serde_json::from_slice(&json_for(&current, 2).unwrap()).unwrap();
    assert_eq!(
        to_v2,
        GizmoV2 {
            label: "z".into(),
            revision: 3
        }
    );

    let to_v3: GizmoV3 = serde_json::from_slice(&json_for(&current, 3).unwrap()).unwrap();
    assert_eq!(
        to_v3,
        GizmoV3 {
            label: "z".into(),
            revision: 3,
            active: true
        }
    );

    let to_v4: GizmoV4 = serde_json::from_slice(&json_for(&current, 4).unwrap()).unwrap();
    assert_eq!(to_v4, current);
}

// ═══════════════════════════════════════════════════════════════════
// Error paths (checked once, against the richest — 3-step — chain)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn error_on_malformed_payload() {
    let err = GizmoV4::migrate_from(4, b"not json", &Json).unwrap_err();
    let MigrationError::DeserializationFailed { target, reason } = &err else {
        panic!("expected DeserializationFailed, got {err:?}");
    };
    assert_eq!(*target, "GizmoV4");
    // The decoder's own diagnosis survives, not just the type name.
    assert!(reason.contains("expected ident"), "{reason}");
    assert!(err
        .to_string()
        .starts_with("failed to parse as GizmoV4: expected ident"));

    // A historical version names its own type.
    let err = GizmoV4::migrate_from(1, b"{}", &Json).unwrap_err();
    assert!(
        matches!(&err, MigrationError::DeserializationFailed { target, reason }
            if *target == "GizmoV1" && reason.contains("missing field")),
        "{err:?}"
    );
}

#[test]
fn error_on_version_too_new() {
    let payload = br#"{"label": "future"}"#;
    let err = GizmoV4::migrate_from(99, payload, &Json).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooNew {
            source: 99,
            current: 4
        }
    );
}

#[test]
fn error_on_source_version_too_old() {
    // A payload version below MIN_VERSION (1) is too old, not too new — it
    // must report VersionTooOld, mirroring the migrate_into path.
    let payload = br#"{"label": "ancient"}"#;
    let err = GizmoV4::migrate_from(0, payload, &Json).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooOld {
            target: 0,
            minimum: 1
        }
    );
}

#[test]
fn error_on_target_version_too_old() {
    let current = GizmoV4 {
        label: "z".into(),
        revision: 3,
        active: true,
    };
    let err = json_for(&current, 0).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooOld {
            target: 0,
            minimum: 1
        }
    );
}

#[test]
fn error_on_target_version_too_new() {
    let current = GizmoV4 {
        label: "z".into(),
        revision: 3,
        active: true,
    };
    let err = json_for(&current, 5).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooNew {
            source: 5,
            current: 4
        }
    );
}

// ═══════════════════════════════════════════════════════════════════
// 4-step chain: Sprocket v1 -> v2 -> v3 -> v4 -> v5
// (proves migration_chain! arity is unbounded)
// ═══════════════════════════════════════════════════════════════════

macro_rules! sprocket_version {
    ($name:ident, $version:literal) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        struct $name {
            teeth: u32,
        }
        impl aimdb_data_contracts::ContractShape for $name {}
        impl SchemaType for $name {
            const NAME: &'static str = "sprocket";
            const VERSION: u32 = $version;
        }
    };
}

sprocket_version!(SprocketV1, 1);
sprocket_version!(SprocketV2, 2);
sprocket_version!(SprocketV3, 3);
sprocket_version!(SprocketV4, 4);
sprocket_version!(SprocketV5, 5);

macro_rules! sprocket_step {
    ($step:ident, $older:ident, $newer:ident, $to_version:literal) => {
        struct $step;
        impl MigrationStep for $step {
            type Older = $older;
            type Newer = $newer;
            const FROM_VERSION: u32 = $to_version - 1;
            const TO_VERSION: u32 = $to_version;
            fn up(v: $older) -> Result<$newer, MigrationError> {
                Ok($newer { teeth: v.teeth })
            }
            fn down(v: $newer) -> Result<$older, MigrationError> {
                Ok($older { teeth: v.teeth })
            }
        }
    };
}

sprocket_step!(SprocketV1ToV2, SprocketV1, SprocketV2, 2);
sprocket_step!(SprocketV2ToV3, SprocketV2, SprocketV3, 3);
sprocket_step!(SprocketV3ToV4, SprocketV3, SprocketV4, 4);
sprocket_step!(SprocketV4ToV5, SprocketV4, SprocketV5, 5);

migration_chain! {
    type Current = SprocketV5;
    steps {
        SprocketV1ToV2: SprocketV1 => SprocketV2,
        SprocketV2ToV3: SprocketV2 => SprocketV3,
        SprocketV3ToV4: SprocketV3 => SprocketV4,
        SprocketV4ToV5: SprocketV4 => SprocketV5,
    }
}

#[test]
fn sprocket_upgrades_from_every_historical_version() {
    for (version, teeth) in [(1u32, 10u32), (2, 11), (3, 12), (4, 13), (5, 14)] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "teeth": teeth,
        }))
        .unwrap();
        let current = SprocketV5::migrate_from(version, &bytes, &Json).unwrap();
        assert_eq!(current, SprocketV5 { teeth });
    }
}

#[test]
fn sprocket_downgrades_to_every_target_version() {
    let current = SprocketV5 { teeth: 20 };
    for target in 1u32..=5 {
        let bytes = json_for(&current, target).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("schema_version").is_none());
        assert_eq!(value["teeth"], 20);
    }
}

// ═══════════════════════════════════════════════════════════════════
// 5-step chain: Doohickey v1 -> v2 -> v3 -> v4 -> v5 -> v6
// ═══════════════════════════════════════════════════════════════════

macro_rules! doohickey_version {
    ($name:ident, $version:literal) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        struct $name {
            size: u32,
        }
        impl aimdb_data_contracts::ContractShape for $name {}
        impl SchemaType for $name {
            const NAME: &'static str = "doohickey";
            const VERSION: u32 = $version;
        }
    };
}

doohickey_version!(DoohickeyV1, 1);
doohickey_version!(DoohickeyV2, 2);
doohickey_version!(DoohickeyV3, 3);
doohickey_version!(DoohickeyV4, 4);
doohickey_version!(DoohickeyV5, 5);
doohickey_version!(DoohickeyV6, 6);

macro_rules! doohickey_step {
    ($step:ident, $older:ident, $newer:ident, $to_version:literal) => {
        struct $step;
        impl MigrationStep for $step {
            type Older = $older;
            type Newer = $newer;
            const FROM_VERSION: u32 = $to_version - 1;
            const TO_VERSION: u32 = $to_version;
            fn up(v: $older) -> Result<$newer, MigrationError> {
                Ok($newer { size: v.size })
            }
            fn down(v: $newer) -> Result<$older, MigrationError> {
                Ok($older { size: v.size })
            }
        }
    };
}

doohickey_step!(DoohickeyV1ToV2, DoohickeyV1, DoohickeyV2, 2);
doohickey_step!(DoohickeyV2ToV3, DoohickeyV2, DoohickeyV3, 3);
doohickey_step!(DoohickeyV3ToV4, DoohickeyV3, DoohickeyV4, 4);
doohickey_step!(DoohickeyV4ToV5, DoohickeyV4, DoohickeyV5, 5);
doohickey_step!(DoohickeyV5ToV6, DoohickeyV5, DoohickeyV6, 6);

migration_chain! {
    type Current = DoohickeyV6;
    steps {
        DoohickeyV1ToV2: DoohickeyV1 => DoohickeyV2,
        DoohickeyV2ToV3: DoohickeyV2 => DoohickeyV3,
        DoohickeyV3ToV4: DoohickeyV3 => DoohickeyV4,
        DoohickeyV4ToV5: DoohickeyV4 => DoohickeyV5,
        DoohickeyV5ToV6: DoohickeyV5 => DoohickeyV6,
    }
}

#[test]
fn doohickey_upgrades_from_every_historical_version() {
    for (version, size) in [(1u32, 1u32), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6)] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "size": size,
        }))
        .unwrap();
        let current = DoohickeyV6::migrate_from(version, &bytes, &Json).unwrap();
        assert_eq!(current, DoohickeyV6 { size });
    }
}

#[test]
fn doohickey_downgrades_to_every_target_version() {
    let current = DoohickeyV6 { size: 99 };
    for target in 1u32..=6 {
        let bytes = json_for(&current, target).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("schema_version").is_none());
        assert_eq!(value["size"], 99);
    }
}
