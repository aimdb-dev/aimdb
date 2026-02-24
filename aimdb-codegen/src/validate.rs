//! Architecture state validator
//!
//! Checks an [`ArchitectureState`] for structural and semantic errors before
//! code is generated or proposals are confirmed.

use crate::state::{ArchitectureState, BufferType};

/// A single validation problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Human-readable description of the problem.
    pub message: String,
    /// Location in state.toml that caused the error (e.g. `records[0].fields[1]`).
    pub location: String,
    /// Whether this blocks code generation (`Error`) or is advisory (`Warning`).
    pub severity: Severity,
}

/// Severity of a [`ValidationError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    /// Blocks code generation — generated code would be invalid or uncompilable.
    Error,
    /// Advisory — generated code may still work but behaviour could be unexpected.
    Warning,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN",
        };
        write!(f, "[{}] {}: {}", tag, self.location, self.message)
    }
}

/// Supported Rust field types for `records.fields[*].type`.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "f64", "f32", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "bool", "String",
];

/// Validate an [`ArchitectureState`] and return all problems found.
///
/// An empty `Vec` means the state is valid and codegen may proceed.
/// Any entry with [`Severity::Error`] should block generation.
pub fn validate(state: &ArchitectureState) -> Vec<ValidationError> {
    let mut errors: Vec<ValidationError> = Vec::new();

    validate_meta(state, &mut errors);
    validate_records(state, &mut errors);

    errors
}

/// Returns `true` if `validate()` produces no `Error`-severity issues.
pub fn is_valid(state: &ArchitectureState) -> bool {
    !validate(state)
        .iter()
        .any(|e| e.severity == Severity::Error)
}

// ── Internal validators ────────────────────────────────────────────────────────

fn validate_meta(state: &ArchitectureState, errors: &mut Vec<ValidationError>) {
    if state.meta.aimdb_version.is_empty() {
        errors.push(ValidationError {
            message: "aimdb_version must not be empty".to_string(),
            location: "meta.aimdb_version".to_string(),
            severity: Severity::Error,
        });
    }
}

fn validate_records(state: &ArchitectureState, errors: &mut Vec<ValidationError>) {
    let mut seen_names: Vec<&str> = Vec::new();

    for (idx, rec) in state.records.iter().enumerate() {
        let loc = format!("records[{idx}]");

        // Name must be non-empty
        if rec.name.is_empty() {
            errors.push(ValidationError {
                message: "record name must not be empty".to_string(),
                location: loc.clone(),
                severity: Severity::Error,
            });
            continue; // Can't do further checks without a name
        }

        // Name should start with an uppercase letter (PascalCase convention)
        if !rec
            .name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            errors.push(ValidationError {
                message: format!(
                    "record name '{}' should start with an uppercase letter (PascalCase)",
                    rec.name
                ),
                location: format!("{loc}.name"),
                severity: Severity::Warning,
            });
        }

        // Duplicate record names
        if seen_names.contains(&rec.name.as_str()) {
            errors.push(ValidationError {
                message: format!("duplicate record name '{}'", rec.name),
                location: format!("{loc}.name"),
                severity: Severity::Error,
            });
        } else {
            seen_names.push(&rec.name);
        }

        // SpmcRing must have capacity > 0
        if rec.buffer == BufferType::SpmcRing {
            match rec.capacity {
                None => {
                    errors.push(ValidationError {
                        message: "SpmcRing requires 'capacity' to be set".to_string(),
                        location: format!("{loc}.capacity"),
                        severity: Severity::Error,
                    });
                }
                Some(0) => {
                    errors.push(ValidationError {
                        message: "SpmcRing capacity must be > 0".to_string(),
                        location: format!("{loc}.capacity"),
                        severity: Severity::Error,
                    });
                }
                _ => {}
            }
        }

        // Warn if capacity is set but buffer is not SpmcRing
        if rec.buffer != BufferType::SpmcRing && rec.capacity.is_some() {
            errors.push(ValidationError {
                message: "capacity is only meaningful for SpmcRing; it will be ignored".to_string(),
                location: format!("{loc}.capacity"),
                severity: Severity::Warning,
            });
        }

        // Warn if no key variants
        if rec.key_variants.is_empty() {
            errors.push(ValidationError {
                message: format!(
                    "record '{}' has no key_variants — the key enum will be empty and unusable",
                    rec.name
                ),
                location: format!("{loc}.key_variants"),
                severity: Severity::Warning,
            });
        }

        // Duplicate key variants
        let mut seen_variants: Vec<&str> = Vec::new();
        for variant in &rec.key_variants {
            if seen_variants.contains(&variant.as_str()) {
                errors.push(ValidationError {
                    message: format!("duplicate key variant '{variant}'"),
                    location: format!("{loc}.key_variants"),
                    severity: Severity::Error,
                });
            } else {
                seen_variants.push(variant);
            }
        }

        // Warn if no fields
        if rec.fields.is_empty() {
            errors.push(ValidationError {
                message: format!(
                    "record '{}' has no fields — the value struct will be empty",
                    rec.name
                ),
                location: format!("{loc}.fields"),
                severity: Severity::Warning,
            });
        }

        // Validate field types
        for (fidx, field) in rec.fields.iter().enumerate() {
            if field.name.is_empty() {
                errors.push(ValidationError {
                    message: "field name must not be empty".to_string(),
                    location: format!("{loc}.fields[{fidx}]"),
                    severity: Severity::Error,
                });
            }
            if !VALID_FIELD_TYPES.contains(&field.field_type.as_str()) {
                errors.push(ValidationError {
                    message: format!(
                        "unsupported field type '{}' — valid types: {}",
                        field.field_type,
                        VALID_FIELD_TYPES.join(", ")
                    ),
                    location: format!("{loc}.fields[{fidx}].type"),
                    severity: Severity::Error,
                });
            }
        }

        // Validate connectors
        for (cidx, conn) in rec.connectors.iter().enumerate() {
            if conn.url.is_empty() {
                errors.push(ValidationError {
                    message: "connector URL must not be empty".to_string(),
                    location: format!("{loc}.connectors[{cidx}].url"),
                    severity: Severity::Error,
                });
            }
            if conn.protocol.is_empty() {
                errors.push(ValidationError {
                    message: "connector protocol must not be empty".to_string(),
                    location: format!("{loc}.connectors[{cidx}].protocol"),
                    severity: Severity::Error,
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ArchitectureState;

    const VALID_TOML: &str = r#"
[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-22T14:00:00Z"
last_modified = "2026-02-22T14:33:00Z"

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
description = "Temperature"

[[records.connectors]]
protocol = "mqtt"
direction = "outbound"
url = "mqtt://sensors/temp/{variant}"
"#;

    fn valid_state() -> ArchitectureState {
        ArchitectureState::from_toml(VALID_TOML).unwrap()
    }

    #[test]
    fn valid_state_has_no_errors() {
        let errs = validate(&valid_state());
        let error_errs: Vec<_> = errs
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(error_errs.is_empty(), "Unexpected errors: {error_errs:?}");
    }

    #[test]
    fn is_valid_returns_true_for_clean_state() {
        assert!(is_valid(&valid_state()));
    }

    #[test]
    fn detects_spmc_missing_capacity() {
        let toml = VALID_TOML.replace("capacity = 256\n", "");
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("capacity"));
        assert!(
            has_err,
            "Should detect missing SpmcRing capacity:\n{errs:?}"
        );
    }

    #[test]
    fn detects_spmc_zero_capacity() {
        let toml = VALID_TOML.replace("capacity = 256", "capacity = 0");
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("capacity must be > 0"));
        assert!(has_err, "Should detect zero capacity:\n{errs:?}");
    }

    #[test]
    fn detects_duplicate_record_names() {
        let toml = format!(
            "{VALID_TOML}{}",
            r#"
[[records]]
name = "TemperatureReading"
buffer = "SingleLatest"
key_variants = ["a"]

[[records.fields]]
name = "value"
type = "f64"
description = "Value"
"#
        );
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("duplicate record name"));
        assert!(has_err, "Should detect duplicate record name:\n{errs:?}");
    }

    #[test]
    fn detects_duplicate_key_variants() {
        let toml = VALID_TOML.replace(
            r#"key_variants = ["indoor", "outdoor"]"#,
            r#"key_variants = ["indoor", "indoor"]"#,
        );
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("duplicate key variant"));
        assert!(has_err, "Should detect duplicate key variants:\n{errs:?}");
    }

    #[test]
    fn detects_invalid_field_type() {
        let toml = VALID_TOML.replace(r#"type = "f64""#, r#"type = "float64""#);
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("unsupported field type"));
        assert!(has_err, "Should detect invalid field type:\n{errs:?}");
    }

    #[test]
    fn detects_empty_connector_url() {
        let toml = VALID_TOML.replace(r#"url = "mqtt://sensors/temp/{variant}""#, r#"url = """#);
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_err = errs
            .iter()
            .any(|e| e.severity == Severity::Error && e.message.contains("URL must not be empty"));
        assert!(has_err, "Should detect empty connector URL:\n{errs:?}");
    }

    #[test]
    fn warning_for_non_pascal_case_name() {
        let toml = VALID_TOML.replace(
            "name = \"TemperatureReading\"",
            "name = \"temperatureReading\"",
        );
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_warn = errs
            .iter()
            .any(|e| e.severity == Severity::Warning && e.message.contains("uppercase"));
        assert!(has_warn, "Should warn about non-PascalCase name:\n{errs:?}");
    }

    #[test]
    fn warning_for_capacity_on_non_spmc() {
        let toml = VALID_TOML.replace("buffer = \"SpmcRing\"", "buffer = \"SingleLatest\"");
        let state = ArchitectureState::from_toml(&toml).unwrap();
        let errs = validate(&state);
        let has_warn = errs.iter().any(|e| {
            e.severity == Severity::Warning && e.message.contains("capacity is only meaningful")
        });
        assert!(
            has_warn,
            "Should warn about capacity on non-SpmcRing:\n{errs:?}"
        );
    }

    #[test]
    fn display_format() {
        let e = ValidationError {
            message: "something wrong".to_string(),
            location: "records[0].name".to_string(),
            severity: Severity::Error,
        };
        let s = format!("{e}");
        assert!(s.contains("[ERROR]"), "Display should show [ERROR]:\n{s}");
        assert!(
            s.contains("records[0].name"),
            "Display should show location:\n{s}"
        );
    }
}
