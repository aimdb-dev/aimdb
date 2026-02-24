//! Mermaid diagram generator
//!
//! Converts an [`ArchitectureState`] into a `flowchart LR` Mermaid diagram
//! following the conventions defined in `.aimdb/CONVENTIONS.md`.

use crate::state::{ArchitectureState, BufferType, ConnectorDirection};

/// Generate a Mermaid `flowchart LR` diagram from architecture state.
///
/// The returned string can be written directly to `.aimdb/architecture.mermaid`.
///
/// # Conventions
/// - Stadium `(["…"])` = SpmcRing
/// - Rounded rect `("…")` = SingleLatest  
/// - Diamond `{"…"}` = Mailbox
/// - Solid arrows → data flow (produce / consume)
/// - Dashed arrows → connector metadata (link_to / link_from)
pub fn generate_mermaid(state: &ArchitectureState) -> String {
    let mut out = String::new();

    out.push_str("flowchart LR\n");

    // ── Record nodes ──────────────────────────────────────────────────────────
    if !state.records.is_empty() {
        out.push_str(
            "\n  %% ── Records ────────────────────────────────────────────────────────────\n",
        );
    }
    for rec in &state.records {
        let node_id = node_id(&rec.name);
        let label = format!("{}\\n{}", rec.name, rec.buffer.label(rec.capacity));
        let node_def = match rec.buffer {
            BufferType::SpmcRing => format!("  {node_id}([\"{label}\"])"),
            BufferType::SingleLatest => format!("  {node_id}(\"{label}\")"),
            BufferType::Mailbox => format!("  {node_id}{{\"{label}\"}}"),
        };
        out.push_str(&node_def);
        out.push('\n');
    }

    // ── Data flow arrows ──────────────────────────────────────────────────────
    if state
        .records
        .iter()
        .any(|r| !r.producers.is_empty() || !r.consumers.is_empty())
    {
        out.push_str(
            "\n  %% ── Data flow (solid arrows) ──────────────────────────────────────────\n",
        );
    }
    for rec in &state.records {
        let nid = node_id(&rec.name);
        for producer in &rec.producers {
            let pid = sanitize_id(producer);
            out.push_str(&format!("  {pid} -->|produce| {nid}\n"));
        }
        for consumer in &rec.consumers {
            let cid = sanitize_id(consumer);
            out.push_str(&format!("  {nid} -->|consume| {cid}\n"));
        }
    }

    // ── Connector metadata (dashed arrows) ────────────────────────────────────
    let has_connectors = state.records.iter().any(|r| !r.connectors.is_empty());
    if has_connectors {
        out.push_str(
            "\n  %% ── Connector metadata (dashed arrows) ────────────────────────────────\n",
        );
        // Collect unique protocol bus node names
        let mut protocols_seen: Vec<String> = Vec::new();
        for rec in &state.records {
            for conn in &rec.connectors {
                let bus = conn.protocol.to_uppercase();
                if !protocols_seen.contains(&bus) {
                    protocols_seen.push(bus);
                }
            }
        }
        for rec in &state.records {
            let nid = node_id(&rec.name);
            for conn in &rec.connectors {
                let bus = conn.protocol.to_uppercase();
                let url = &conn.url;
                match conn.direction {
                    ConnectorDirection::Outbound => {
                        out.push_str(&format!("  {nid} -.->|\"link_to {url}\"| {bus}\n"));
                    }
                    ConnectorDirection::Inbound => {
                        out.push_str(&format!("  {bus} -.->|\"link_from {url}\"| {nid}\n"));
                    }
                }
            }
        }
    }

    out
}

/// Derive a stable Mermaid node ID from a record name.
///
/// Converts PascalCase to SCREAMING_SNAKE_CASE, e.g.
/// `TemperatureReading` → `TEMPERATURE_READING`.
pub fn node_id(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit())
        {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// Sanitize an arbitrary identifier for use as a Mermaid node ID.
///
/// Replaces hyphens and spaces with underscores, removes other non-alphanumeric chars.
fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ArchitectureState;

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

[[records.connectors]]
protocol = "mqtt"
direction = "inbound"
url = "mqtt://ota/cmd/{variant}"

[[records]]
name = "FirmwareVersion"
buffer = "SingleLatest"
key_prefix = "device.firmware."
key_variants = ["gateway-01"]
producers = ["cloud_service"]
consumers = ["updater"]

[[records.fields]]
name = "version"
type = "String"
description = "Semantic version"
"#;

    fn state() -> ArchitectureState {
        ArchitectureState::from_toml(SAMPLE_TOML).unwrap()
    }

    #[test]
    fn contains_flowchart_header() {
        let out = generate_mermaid(&state());
        assert!(
            out.starts_with("flowchart LR\n"),
            "Must start with flowchart LR"
        );
    }

    #[test]
    fn spmc_ring_uses_stadium_shape() {
        let out = generate_mermaid(&state());
        // Stadium: ([" ... "])
        assert!(
            out.contains("TEMPERATURE_READING([\"TemperatureReading\\nSpmcRing · 256\"])"),
            "SpmcRing node should use stadium shape:\n{out}"
        );
    }

    #[test]
    fn mailbox_uses_diamond_shape() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains("OTA_COMMAND{\"OtaCommand\\nMailbox\"}"),
            "Mailbox node should use diamond shape:\n{out}"
        );
    }

    #[test]
    fn single_latest_uses_rounded_rect() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains("FIRMWARE_VERSION(\"FirmwareVersion\\nSingleLatest\")"),
            "SingleLatest node should use rounded rect:\n{out}"
        );
    }

    #[test]
    fn produce_arrows_present() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains("sensor_task -->|produce| TEMPERATURE_READING"),
            "Producer arrow missing:\n{out}"
        );
    }

    #[test]
    fn consume_arrows_present() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains("TEMPERATURE_READING -->|consume| dashboard"),
            "Consumer arrow missing:\n{out}"
        );
        assert!(
            out.contains("TEMPERATURE_READING -->|consume| anomaly_detector"),
            "Consumer arrow missing:\n{out}"
        );
    }

    #[test]
    fn outbound_connector_dashed_arrow() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains(
                "TEMPERATURE_READING -.->|\"link_to mqtt://sensors/temp/{variant}\"| MQTT"
            ),
            "Outbound dashed arrow missing:\n{out}"
        );
    }

    #[test]
    fn inbound_connector_dashed_arrow() {
        let out = generate_mermaid(&state());
        assert!(
            out.contains("MQTT -.->|\"link_from mqtt://ota/cmd/{variant}\"| OTA_COMMAND"),
            "Inbound dashed arrow missing:\n{out}"
        );
    }

    #[test]
    fn node_id_pascal_to_screaming_snake() {
        assert_eq!(node_id("TemperatureReading"), "TEMPERATURE_READING");
        assert_eq!(node_id("OtaCommand"), "OTA_COMMAND");
        assert_eq!(node_id("FirmwareVersion"), "FIRMWARE_VERSION");
        assert_eq!(node_id("AppConfig"), "APP_CONFIG");
        assert_eq!(node_id("Temp"), "TEMP");
    }
}
