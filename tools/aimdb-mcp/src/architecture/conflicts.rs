//! Conflict detection between `.aimdb/state.toml` and a running AimDB instance.
//!
//! Compares the agent's declared architecture against what is live in the
//! database, categorised by severity.

use aimdb_codegen::{ArchitectureState, BufferType};
use serde::{Deserialize, Serialize};

/// A detected discrepancy between state.toml and the running instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// Record name (may be from state or instance).
    pub record_name: String,
    /// Category of conflict.
    pub conflict_type: ConflictType,
    /// Human-readable explanation.
    pub message: String,
    /// Severity of the conflict.
    pub severity: ConflictSeverity,
}

/// Categories of conflict (mirrors the spec table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    /// In state.toml but not in running instance.
    MissingInInstance,
    /// In running instance but not in state.toml.
    MissingInState,
    /// Buffer type differs.
    BufferMismatch,
    /// Buffer capacity differs.
    CapacityMismatch,
    /// Connector URL differs.
    ConnectorMismatch,
}

/// Conflict severity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSeverity {
    /// Likely a stale build or misconfiguration — blocks proposal confirmation.
    Error,
    /// Possibly intentional — surface but don't block.
    Warning,
    /// Informational — manually registered record, not agent-managed.
    Info,
}

/// Summary of conflict detection results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictReport {
    pub conflicts: Vec<Conflict>,
    pub error_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
    pub in_sync: bool,
}

impl ConflictReport {
    /// Build from a list of conflicts.
    pub fn from_conflicts(conflicts: Vec<Conflict>) -> Self {
        let error_count = conflicts
            .iter()
            .filter(|c| c.severity == ConflictSeverity::Error)
            .count();
        let warning_count = conflicts
            .iter()
            .filter(|c| c.severity == ConflictSeverity::Warning)
            .count();
        let info_count = conflicts
            .iter()
            .filter(|c| c.severity == ConflictSeverity::Info)
            .count();
        let in_sync = error_count == 0 && warning_count == 0;
        Self {
            conflicts,
            error_count,
            warning_count,
            info_count,
            in_sync,
        }
    }
}

/// Lightweight description of a record as reported by the running instance.
#[derive(Debug, Clone)]
pub struct LiveRecord {
    /// The record's key string (e.g. `"sensors.temp.indoor"`).
    pub name: String,
    /// Buffer type string as returned by AimX: "spmc_ring", "single_latest", "mailbox".
    pub buffer_type: String,
    /// Buffer capacity, if applicable.
    pub buffer_capacity: Option<usize>,
}

/// Map a state.toml `BufferType` to the AimX wire string.
fn state_buffer_to_wire(bt: &BufferType) -> &'static str {
    match bt {
        BufferType::SpmcRing => "spmc_ring",
        BufferType::SingleLatest => "single_latest",
        BufferType::Mailbox => "mailbox",
    }
}

/// Detect conflicts between `state` and the records visible in the running instance.
///
/// `live_records` is the list of records returned by `list_records` for the
/// connected instance (across all key variants). Because state.toml uses a
/// prefix + variants model, we match by checking whether the live record name
/// starts with the `key_prefix` of each state record.
pub fn detect_conflicts(state: &ArchitectureState, live_records: &[LiveRecord]) -> ConflictReport {
    let mut conflicts: Vec<Conflict> = Vec::new();

    // ── Records in state but not (or mismatched) in instance ─────────────────
    for rec in &state.records {
        // Collect live records that belong to this state record's key family
        // (i.e. their name starts with key_prefix, or matches a full variant key)
        let matching_live: Vec<&LiveRecord> = live_records
            .iter()
            .filter(|lr| {
                if rec.key_prefix.is_empty() {
                    // Match exact name or any variant
                    rec.key_variants.contains(&lr.name) || lr.name == rec.name
                } else {
                    lr.name.starts_with(&rec.key_prefix)
                }
            })
            .collect();

        if matching_live.is_empty() {
            conflicts.push(Conflict {
                record_name: rec.name.clone(),
                conflict_type: ConflictType::MissingInInstance,
                message: format!(
                    "'{}' is declared in state.toml but not found in the running instance. \
                     Codegen may not have run yet, or the binary has not been redeployed.",
                    rec.name
                ),
                severity: ConflictSeverity::Warning,
            });
            continue;
        }

        let expected_buffer_wire = state_buffer_to_wire(&rec.buffer);

        for lr in &matching_live {
            // Buffer type mismatch
            if lr.buffer_type != expected_buffer_wire && lr.buffer_type != "none" {
                conflicts.push(Conflict {
                    record_name: lr.name.clone(),
                    conflict_type: ConflictType::BufferMismatch,
                    message: format!(
                        "'{}': state.toml expects buffer '{}' but instance reports '{}'. \
                         Likely a stale build — rerun `aimdb generate && cargo build`.",
                        lr.name, expected_buffer_wire, lr.buffer_type
                    ),
                    severity: ConflictSeverity::Error,
                });
                continue;
            }

            // Capacity mismatch (SpmcRing only)
            if rec.buffer == BufferType::SpmcRing {
                if let (Some(expected), Some(actual)) = (rec.capacity, lr.buffer_capacity) {
                    if expected != actual {
                        conflicts.push(Conflict {
                            record_name: lr.name.clone(),
                            conflict_type: ConflictType::CapacityMismatch,
                            message: format!(
                                "'{}': state.toml declares SpmcRing capacity {} but \
                                 instance reports capacity {}. \
                                 May be an intentional application-level override.",
                                lr.name, expected, actual
                            ),
                            severity: ConflictSeverity::Warning,
                        });
                    }
                }
            }
        }
    }

    // ── Records in instance but not in state ──────────────────────────────────
    for lr in live_records {
        let present_in_state = state.records.iter().any(|rec| {
            if rec.key_prefix.is_empty() {
                rec.key_variants.contains(&lr.name) || lr.name == rec.name
            } else {
                lr.name.starts_with(&rec.key_prefix)
            }
        });
        if !present_in_state {
            conflicts.push(Conflict {
                record_name: lr.name.clone(),
                conflict_type: ConflictType::MissingInState,
                message: format!(
                    "'{}' exists in the running instance but is not declared in state.toml. \
                     It may be a manually registered record not managed by the architecture agent.",
                    lr.name
                ),
                severity: ConflictSeverity::Info,
            });
        }
    }

    ConflictReport::from_conflicts(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_codegen::ArchitectureState;

    const STATE_TOML: &str = r#"
[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-22T14:00:00Z"
last_modified = "2026-02-22T14:00:00Z"

[[records]]
name = "TemperatureReading"
buffer = "SpmcRing"
capacity = 256
key_prefix = "sensors.temp."
key_variants = ["indoor", "outdoor"]
producers = ["sensor_task"]
consumers = ["dashboard"]

[[records.fields]]
name = "celsius"
type = "f64"
description = "Celsius"
"#;

    fn state() -> ArchitectureState {
        ArchitectureState::from_toml(STATE_TOML).unwrap()
    }

    #[test]
    fn in_sync_when_buffer_matches() {
        let live = vec![
            LiveRecord {
                name: "sensors.temp.indoor".to_string(),
                buffer_type: "spmc_ring".to_string(),
                buffer_capacity: Some(256),
            },
            LiveRecord {
                name: "sensors.temp.outdoor".to_string(),
                buffer_type: "spmc_ring".to_string(),
                buffer_capacity: Some(256),
            },
        ];
        let report = detect_conflicts(&state(), &live);
        assert!(report.in_sync, "Should be in sync: {:?}", report.conflicts);
    }

    #[test]
    fn detects_missing_in_instance() {
        let report = detect_conflicts(&state(), &[]);
        assert_eq!(report.warning_count, 1);
        assert!(report.conflicts[0].conflict_type == ConflictType::MissingInInstance);
    }

    #[test]
    fn detects_buffer_mismatch() {
        let live = vec![LiveRecord {
            name: "sensors.temp.indoor".to_string(),
            buffer_type: "single_latest".to_string(),
            buffer_capacity: None,
        }];
        let report = detect_conflicts(&state(), &live);
        let has_mismatch = report
            .conflicts
            .iter()
            .any(|c| c.conflict_type == ConflictType::BufferMismatch);
        assert!(
            has_mismatch,
            "Should detect buffer mismatch: {:?}",
            report.conflicts
        );
        assert!(report.error_count > 0);
    }

    #[test]
    fn detects_capacity_mismatch() {
        let live = vec![LiveRecord {
            name: "sensors.temp.indoor".to_string(),
            buffer_type: "spmc_ring".to_string(),
            buffer_capacity: Some(1024),
        }];
        let report = detect_conflicts(&state(), &live);
        let has_cap = report
            .conflicts
            .iter()
            .any(|c| c.conflict_type == ConflictType::CapacityMismatch);
        assert!(
            has_cap,
            "Should detect capacity mismatch: {:?}",
            report.conflicts
        );
    }

    #[test]
    fn detects_missing_in_state() {
        let live = vec![
            LiveRecord {
                name: "sensors.temp.indoor".to_string(),
                buffer_type: "spmc_ring".to_string(),
                buffer_capacity: Some(256),
            },
            LiveRecord {
                name: "some.other.record".to_string(),
                buffer_type: "single_latest".to_string(),
                buffer_capacity: None,
            },
        ];
        let report = detect_conflicts(&state(), &live);
        let has_missing = report
            .conflicts
            .iter()
            .any(|c| c.conflict_type == ConflictType::MissingInState);
        assert!(
            has_missing,
            "Should detect missing_in_state: {:?}",
            report.conflicts
        );
        assert_eq!(report.info_count, 1);
    }
}
