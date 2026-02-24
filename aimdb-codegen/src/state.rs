//! AimDB Codegen — architecture state types and TOML parser
//!
//! Deserialises `.aimdb/state.toml` into [`ArchitectureState`].

use serde::{Deserialize, Serialize};

// ── Top-level state ──────────────────────────────────────────────────────────

/// The full contents of `.aimdb/state.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureState {
    pub meta: Meta,
    #[serde(default)]
    pub records: Vec<RecordDef>,
    #[serde(default)]
    pub decisions: Vec<DecisionEntry>,
}

impl ArchitectureState {
    /// Parse from a TOML string (the contents of `state.toml`).
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Serialise back to a TOML string.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

// ── Meta block ───────────────────────────────────────────────────────────────

/// `[meta]` block — version and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub aimdb_version: String,
    pub created_at: String,
    pub last_modified: String,
}

// ── Record definition ────────────────────────────────────────────────────────

/// One `[[records]]` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordDef {
    /// PascalCase name, e.g. `TemperatureReading`.
    pub name: String,
    /// Buffer type selection.
    pub buffer: BufferType,
    /// Required when `buffer == SpmcRing`. Ignored otherwise.
    #[serde(default)]
    pub capacity: Option<usize>,
    /// Common key prefix, e.g. `"sensors.temp."`.
    #[serde(default)]
    pub key_prefix: String,
    /// Concrete key variant strings, e.g. `["indoor", "outdoor", "garage"]`.
    #[serde(default)]
    pub key_variants: Vec<String>,
    /// Names of tasks that produce values into this record.
    #[serde(default)]
    pub producers: Vec<String>,
    /// Names of tasks that consume values from this record.
    #[serde(default)]
    pub consumers: Vec<String>,
    /// Value struct fields (agent-derived from datasheets / specs / conversation).
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    /// External connector definitions.
    #[serde(default)]
    pub connectors: Vec<ConnectorDef>,
}

// ── Buffer type ──────────────────────────────────────────────────────────────

/// The three AimDB buffer primitives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BufferType {
    SpmcRing,
    SingleLatest,
    Mailbox,
}

impl BufferType {
    /// Human-readable label used in Mermaid node annotations.
    pub fn label(&self, capacity: Option<usize>) -> String {
        match self {
            BufferType::SpmcRing => {
                let cap = capacity.unwrap_or(256);
                format!("SpmcRing · {cap}")
            }
            BufferType::SingleLatest => "SingleLatest".to_string(),
            BufferType::Mailbox => "Mailbox".to_string(),
        }
    }

    /// The `BufferCfg` expression emitted into generated Rust.
    pub fn rust_expr(&self, capacity: Option<usize>) -> String {
        match self {
            BufferType::SpmcRing => {
                let cap = capacity.unwrap_or(256);
                format!("BufferCfg::SpmcRing {{ capacity: {cap} }}")
            }
            BufferType::SingleLatest => "BufferCfg::SingleLatest".to_string(),
            BufferType::Mailbox => "BufferCfg::Mailbox".to_string(),
        }
    }

    /// The `BufferCfg` expression as a token stream for use with `quote!`.
    pub fn to_tokens(&self, capacity: Option<usize>) -> proc_macro2::TokenStream {
        use quote::quote;
        match self {
            BufferType::SpmcRing => {
                let cap = proc_macro2::Literal::usize_unsuffixed(capacity.unwrap_or(256));
                quote! { BufferCfg::SpmcRing { capacity: #cap } }
            }
            BufferType::SingleLatest => quote! { BufferCfg::SingleLatest },
            BufferType::Mailbox => quote! { BufferCfg::Mailbox },
        }
    }
}

// ── Field definition ─────────────────────────────────────────────────────────

/// One `[[records.fields]]` entry — a typed field in the value struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    /// Rust primitive type string, e.g. `"f64"`, `"u64"`, `"String"`, `"bool"`.
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub description: String,
}

// ── Connector definition ─────────────────────────────────────────────────────

/// One `[[records.connectors]]` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorDef {
    /// Protocol identifier lower-case, e.g. `"mqtt"`, `"knx"`.
    pub protocol: String,
    /// `"outbound"` → `link_to`, `"inbound"` → `link_from`.
    pub direction: ConnectorDirection,
    /// URL template, may contain `{variant}` placeholder.
    pub url: String,
}

/// Connector data flow direction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorDirection {
    Outbound,
    Inbound,
}

// ── Decision log entry ───────────────────────────────────────────────────────

/// One `[[decisions]]` entry — architectural rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub record: String,
    pub field: String,
    pub chosen: String,
    pub alternative: String,
    pub reason: String,
    pub timestamp: String,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-22T14:00:00Z"
last_modified = "2026-02-22T14:33:00Z"

[[records]]
name = "TemperatureReading"
buffer = "SpmcRing"
capacity = 256
key_prefix = "sensors.temp."
key_variants = ["indoor", "outdoor", "garage"]
producers = ["sensor_task"]
consumers = ["dashboard", "anomaly_detector"]

[[records.fields]]
name = "celsius"
type = "f64"
description = "Temperature in degrees Celsius"

[[records.fields]]
name = "humidity_percent"
type = "f64"
description = "Relative humidity 0-100"

[[records.fields]]
name = "timestamp"
type = "u64"
description = "Unix timestamp in milliseconds"

[[records.connectors]]
protocol = "mqtt"
direction = "outbound"
url = "mqtt://sensors/temp/{variant}"

[[records]]
name = "OtaCommand"
buffer = "Mailbox"
key_prefix = "device.ota."
key_variants = ["gateway-01"]
producers = ["cloud_ota_service"]
consumers = ["device_update_task"]

[[records.fields]]
name = "action"
type = "String"
description = "Command action"

[[decisions]]
record = "TemperatureReading"
field = "buffer"
chosen = "SpmcRing"
alternative = "SingleLatest"
reason = "Anomaly detector needs a sample window"
timestamp = "2026-02-22T14:20:00Z"
"#;

    #[test]
    fn parses_meta() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(state.meta.aimdb_version, "0.5.0");
        assert_eq!(state.meta.created_at, "2026-02-22T14:00:00Z");
    }

    #[test]
    fn parses_records() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(state.records.len(), 2);

        let r = &state.records[0];
        assert_eq!(r.name, "TemperatureReading");
        assert_eq!(r.buffer, BufferType::SpmcRing);
        assert_eq!(r.capacity, Some(256));
        assert_eq!(r.key_prefix, "sensors.temp.");
        assert_eq!(r.key_variants, vec!["indoor", "outdoor", "garage"]);
        assert_eq!(r.producers, vec!["sensor_task"]);
        assert_eq!(r.consumers, vec!["dashboard", "anomaly_detector"]);
    }

    #[test]
    fn parses_fields() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        let r = &state.records[0];
        assert_eq!(r.fields.len(), 3);
        assert_eq!(r.fields[0].name, "celsius");
        assert_eq!(r.fields[0].field_type, "f64");
        assert_eq!(r.fields[0].description, "Temperature in degrees Celsius");
    }

    #[test]
    fn parses_connectors() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        let r = &state.records[0];
        assert_eq!(r.connectors.len(), 1);
        assert_eq!(r.connectors[0].protocol, "mqtt");
        assert_eq!(r.connectors[0].direction, ConnectorDirection::Outbound);
        assert_eq!(r.connectors[0].url, "mqtt://sensors/temp/{variant}");
    }

    #[test]
    fn parses_decisions() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(state.decisions.len(), 1);
        assert_eq!(state.decisions[0].record, "TemperatureReading");
        assert_eq!(state.decisions[0].chosen, "SpmcRing");
    }

    #[test]
    fn buffer_label_spmc() {
        assert_eq!(BufferType::SpmcRing.label(Some(256)), "SpmcRing · 256");
    }

    #[test]
    fn buffer_label_single_latest() {
        assert_eq!(BufferType::SingleLatest.label(None), "SingleLatest");
    }

    #[test]
    fn buffer_rust_expr_mailbox() {
        assert_eq!(BufferType::Mailbox.rust_expr(None), "BufferCfg::Mailbox");
    }

    #[test]
    fn round_trips_toml() {
        let state = ArchitectureState::from_toml(SAMPLE_TOML).unwrap();
        let serialised = state.to_toml().unwrap();
        let state2 = ArchitectureState::from_toml(&serialised).unwrap();
        assert_eq!(state.records.len(), state2.records.len());
        assert_eq!(state.decisions.len(), state2.decisions.len());
    }
}
