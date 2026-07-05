//! Round-trip behavioral coverage for `migration_chain!` at the current
//! arities (1-, 2-, and 3-step). This is the regression pin design 039's
//! PR2 proc-macro rewrite must reproduce unchanged, and PR2/PR3 extend
//! (4-/5-step chains, error paths) without touching these cases.
//!
//! Lives under `tests/` (a separate crate) rather than `#[cfg(test)] mod
//! tests` inside `src/migratable.rs`: after the PR2 rewrite the macro emits
//! absolute `::aimdb_data_contracts::...` paths (matching the `RecordKey` /
//! `aimdb_core` precedent in `aimdb-derive`), which only resolve from
//! outside the defining crate.

#![cfg(feature = "migratable")]

use aimdb_data_contracts::{
    migration_chain, MigrationChain, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════
// 1-step chain: Widget v1 -> v2
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WidgetV1 {
    #[serde(default = "widget_v1")]
    schema_version: u32,
    count: u32,
}
fn widget_v1() -> u32 {
    1
}
impl SchemaType for WidgetV1 {
    const NAME: &'static str = "widget";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WidgetV2 {
    #[serde(default = "widget_v2")]
    schema_version: u32,
    count: u64,
}
fn widget_v2() -> u32 {
    2
}
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
            schema_version: 2,
            count: u64::from(v1.count),
        })
    }
    fn down(v2: WidgetV2) -> Result<WidgetV1, MigrationError> {
        Ok(WidgetV1 {
            schema_version: 1,
            count: v2.count as u32,
        })
    }
}

migration_chain! {
    type Current = WidgetV2;
    version_field = "schema_version";
    steps {
        WidgetV1ToV2: WidgetV1 => WidgetV2,
    }
}

#[test]
fn widget_upgrades_from_v1() {
    let v1 = WidgetV1 {
        schema_version: 1,
        count: 7,
    };
    let bytes = serde_json::to_vec(&v1).unwrap();
    let current = WidgetV2::migrate_from_bytes(&bytes).unwrap();
    assert_eq!(
        current,
        WidgetV2 {
            schema_version: 2,
            count: 7
        }
    );
}

#[test]
fn widget_parses_current_version_directly() {
    let v2 = WidgetV2 {
        schema_version: 2,
        count: 42,
    };
    let bytes = serde_json::to_vec(&v2).unwrap();
    let current = WidgetV2::migrate_from_bytes(&bytes).unwrap();
    assert_eq!(current, v2);
}

#[test]
fn widget_downgrades_to_v1() {
    let v2 = WidgetV2 {
        schema_version: 2,
        count: 9,
    };
    let bytes = v2.migrate_to_version(1).unwrap();
    let v1: WidgetV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v1,
        WidgetV1 {
            schema_version: 1,
            count: 9
        }
    );
}

#[test]
fn widget_serializes_current_version_directly() {
    let v2 = WidgetV2 {
        schema_version: 2,
        count: 5,
    };
    let bytes = v2.migrate_to_version(2).unwrap();
    let round_tripped: WidgetV2 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(round_tripped, v2);
}

// ═══════════════════════════════════════════════════════════════════
// 2-step chain: Gadget v1 -> v2 -> v3
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV1 {
    #[serde(default = "gadget_v1")]
    schema_version: u32,
    watts: u32,
}
fn gadget_v1() -> u32 {
    1
}
impl SchemaType for GadgetV1 {
    const NAME: &'static str = "gadget";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV2 {
    #[serde(default = "gadget_v2")]
    schema_version: u32,
    milliwatts: u32,
}
fn gadget_v2() -> u32 {
    2
}
impl SchemaType for GadgetV2 {
    const NAME: &'static str = "gadget";
    const VERSION: u32 = 2;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GadgetV3 {
    #[serde(default = "gadget_v3")]
    schema_version: u32,
    milliwatts: u64,
}
fn gadget_v3() -> u32 {
    3
}
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
            schema_version: 2,
            milliwatts: v1.watts * 1000,
        })
    }
    fn down(v2: GadgetV2) -> Result<GadgetV1, MigrationError> {
        Ok(GadgetV1 {
            schema_version: 1,
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
            schema_version: 3,
            milliwatts: u64::from(v2.milliwatts),
        })
    }
    fn down(v3: GadgetV3) -> Result<GadgetV2, MigrationError> {
        Ok(GadgetV2 {
            schema_version: 2,
            milliwatts: v3.milliwatts as u32,
        })
    }
}

migration_chain! {
    type Current = GadgetV3;
    version_field = "schema_version";
    steps {
        GadgetV1ToV2: GadgetV1 => GadgetV2,
        GadgetV2ToV3: GadgetV2 => GadgetV3,
    }
}

#[test]
fn gadget_upgrades_from_v1() {
    let v1 = GadgetV1 {
        schema_version: 1,
        watts: 2,
    };
    let bytes = serde_json::to_vec(&v1).unwrap();
    let current = GadgetV3::migrate_from_bytes(&bytes).unwrap();
    assert_eq!(
        current,
        GadgetV3 {
            schema_version: 3,
            milliwatts: 2000
        }
    );
}

#[test]
fn gadget_upgrades_from_v2() {
    let v2 = GadgetV2 {
        schema_version: 2,
        milliwatts: 1500,
    };
    let bytes = serde_json::to_vec(&v2).unwrap();
    let current = GadgetV3::migrate_from_bytes(&bytes).unwrap();
    assert_eq!(
        current,
        GadgetV3 {
            schema_version: 3,
            milliwatts: 1500
        }
    );
}

#[test]
fn gadget_downgrades_to_v1() {
    let v3 = GadgetV3 {
        schema_version: 3,
        milliwatts: 4000,
    };
    let bytes = v3.migrate_to_version(1).unwrap();
    let v1: GadgetV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v1,
        GadgetV1 {
            schema_version: 1,
            watts: 4
        }
    );
}

#[test]
fn gadget_downgrades_to_v2() {
    let v3 = GadgetV3 {
        schema_version: 3,
        milliwatts: 4000,
    };
    let bytes = v3.migrate_to_version(2).unwrap();
    let v2: GadgetV2 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v2,
        GadgetV2 {
            schema_version: 2,
            milliwatts: 4000
        }
    );
}

// ═══════════════════════════════════════════════════════════════════
// 3-step chain: Gizmo v1 -> v2 -> v3 -> v4
// ═══════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV1 {
    #[serde(default = "gizmo_v1")]
    schema_version: u32,
    label: String,
}
fn gizmo_v1() -> u32 {
    1
}
impl SchemaType for GizmoV1 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV2 {
    #[serde(default = "gizmo_v2")]
    schema_version: u32,
    label: String,
    revision: u32,
}
fn gizmo_v2() -> u32 {
    2
}
impl SchemaType for GizmoV2 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 2;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV3 {
    #[serde(default = "gizmo_v3")]
    schema_version: u32,
    label: String,
    revision: u32,
    active: bool,
}
fn gizmo_v3() -> u32 {
    3
}
impl SchemaType for GizmoV3 {
    const NAME: &'static str = "gizmo";
    const VERSION: u32 = 3;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GizmoV4 {
    #[serde(default = "gizmo_v4")]
    schema_version: u32,
    label: String,
    revision: u64,
    active: bool,
}
fn gizmo_v4() -> u32 {
    4
}
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
            schema_version: 2,
            label: v1.label,
            revision: 0,
        })
    }
    fn down(v2: GizmoV2) -> Result<GizmoV1, MigrationError> {
        Ok(GizmoV1 {
            schema_version: 1,
            label: v2.label,
        })
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
            schema_version: 3,
            label: v2.label,
            revision: v2.revision,
            active: true,
        })
    }
    fn down(v3: GizmoV3) -> Result<GizmoV2, MigrationError> {
        Ok(GizmoV2 {
            schema_version: 2,
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
            schema_version: 4,
            label: v3.label,
            revision: u64::from(v3.revision),
            active: v3.active,
        })
    }
    fn down(v4: GizmoV4) -> Result<GizmoV3, MigrationError> {
        Ok(GizmoV3 {
            schema_version: 3,
            label: v4.label,
            revision: v4.revision as u32,
            active: v4.active,
        })
    }
}

migration_chain! {
    type Current = GizmoV4;
    version_field = "schema_version";
    steps {
        GizmoV1ToV2: GizmoV1 => GizmoV2,
        GizmoV2ToV3: GizmoV2 => GizmoV3,
        GizmoV3ToV4: GizmoV3 => GizmoV4,
    }
}

#[test]
fn gizmo_upgrades_from_every_historical_version() {
    let v1 = GizmoV1 {
        schema_version: 1,
        label: "a".into(),
    };
    let from_v1 = GizmoV4::migrate_from_bytes(&serde_json::to_vec(&v1).unwrap()).unwrap();
    assert_eq!(
        from_v1,
        GizmoV4 {
            schema_version: 4,
            label: "a".into(),
            revision: 0,
            active: true
        }
    );

    let v2 = GizmoV2 {
        schema_version: 2,
        label: "b".into(),
        revision: 5,
    };
    let from_v2 = GizmoV4::migrate_from_bytes(&serde_json::to_vec(&v2).unwrap()).unwrap();
    assert_eq!(
        from_v2,
        GizmoV4 {
            schema_version: 4,
            label: "b".into(),
            revision: 5,
            active: true
        }
    );

    let v3 = GizmoV3 {
        schema_version: 3,
        label: "c".into(),
        revision: 9,
        active: false,
    };
    let from_v3 = GizmoV4::migrate_from_bytes(&serde_json::to_vec(&v3).unwrap()).unwrap();
    assert_eq!(
        from_v3,
        GizmoV4 {
            schema_version: 4,
            label: "c".into(),
            revision: 9,
            active: false
        }
    );

    let v4 = GizmoV4 {
        schema_version: 4,
        label: "d".into(),
        revision: 12,
        active: true,
    };
    let from_v4 = GizmoV4::migrate_from_bytes(&serde_json::to_vec(&v4).unwrap()).unwrap();
    assert_eq!(from_v4, v4);
}

#[test]
fn gizmo_downgrades_to_every_target_version() {
    let current = GizmoV4 {
        schema_version: 4,
        label: "z".into(),
        revision: 3,
        active: true,
    };

    let to_v1: GizmoV1 = serde_json::from_slice(&current.migrate_to_version(1).unwrap()).unwrap();
    assert_eq!(
        to_v1,
        GizmoV1 {
            schema_version: 1,
            label: "z".into()
        }
    );

    let to_v2: GizmoV2 = serde_json::from_slice(&current.migrate_to_version(2).unwrap()).unwrap();
    assert_eq!(
        to_v2,
        GizmoV2 {
            schema_version: 2,
            label: "z".into(),
            revision: 3
        }
    );

    let to_v3: GizmoV3 = serde_json::from_slice(&current.migrate_to_version(3).unwrap()).unwrap();
    assert_eq!(
        to_v3,
        GizmoV3 {
            schema_version: 3,
            label: "z".into(),
            revision: 3,
            active: true
        }
    );

    let to_v4: GizmoV4 = serde_json::from_slice(&current.migrate_to_version(4).unwrap()).unwrap();
    assert_eq!(to_v4, current);
}

// ═══════════════════════════════════════════════════════════════════
// Error paths (checked once, against the richest — 3-step — chain)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn error_on_malformed_payload() {
    let err = GizmoV4::migrate_from_bytes(b"not json").unwrap_err();
    assert_eq!(err, MigrationError::DeserializationFailed("invalid JSON"));
}

#[test]
fn error_on_missing_version_field() {
    let err = GizmoV4::migrate_from_bytes(br#"{"label": "no version"}"#).unwrap_err();
    assert_eq!(err, MigrationError::MissingVersion);
}

#[test]
fn error_on_version_too_new() {
    let payload = br#"{"schema_version": 99, "label": "future"}"#;
    let err = GizmoV4::migrate_from_bytes(payload).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooNew {
            source: 99,
            current: 4
        }
    );
}

#[test]
fn error_on_target_version_too_old() {
    let current = GizmoV4 {
        schema_version: 4,
        label: "z".into(),
        revision: 3,
        active: true,
    };
    let err = current.migrate_to_version(0).unwrap_err();
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
        schema_version: 4,
        label: "z".into(),
        revision: 3,
        active: true,
    };
    let err = current.migrate_to_version(5).unwrap_err();
    assert_eq!(
        err,
        MigrationError::VersionTooNew {
            source: 5,
            current: 4
        }
    );
}
