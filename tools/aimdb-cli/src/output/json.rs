//! JSON Output Formatting

use aimdb_client::discovery::InstanceInfo;
use aimdb_client::protocol::RecordMetadata;
use serde::Serialize;

/// Format data as pretty JSON
pub fn format_json_pretty<T: Serialize + ?Sized>(data: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(data)
}

/// Format data as compact JSON (one line)
pub fn format_json_compact<T: Serialize + ?Sized>(data: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(data)
}

/// Serializable form of discovered instances, shared by the JSON and YAML
/// output so both use the same field names (`InstanceInfo` itself does not
/// implement `Serialize`).
pub fn instances_value(instances: &[InstanceInfo]) -> Vec<serde_json::Value> {
    instances
        .iter()
        .map(|i| {
            serde_json::json!({
                "socket_path": i.endpoint.display().to_string(),
                "server_version": i.server_version,
                "protocol_version": i.protocol_version,
                "permissions": i.permissions,
                "writable_records": i.writable_records,
                "max_subscriptions": i.max_subscriptions,
                "authenticated": i.authenticated,
            })
        })
        .collect()
}

/// Format instances as JSON
pub fn format_instances_json(
    instances: &[InstanceInfo],
    pretty: bool,
) -> Result<String, serde_json::Error> {
    let data = instances_value(instances);

    if pretty {
        format_json_pretty(&data)
    } else {
        format_json_compact(&data)
    }
}

/// Format records as JSON
pub fn format_records_json(
    records: &[RecordMetadata],
    pretty: bool,
) -> Result<String, serde_json::Error> {
    if pretty {
        format_json_pretty(records)
    } else {
        format_json_compact(records)
    }
}

/// Generic JSON formatting for any serializable type
pub fn format_json<T: Serialize + ?Sized>(
    data: &T,
    pretty: bool,
) -> Result<String, serde_json::Error> {
    if pretty {
        format_json_pretty(data)
    } else {
        format_json_compact(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_formatting() {
        use aimdb_core::graph::RecordOrigin;
        use aimdb_core::record_id::{RecordId, StringKey};
        use core::any::TypeId;

        let records = vec![RecordMetadata::new(
            RecordId::new(0),
            StringKey::new("sensor.temperature"),
            TypeId::of::<i32>(),
            "Temperature".to_string(),
            RecordOrigin::Source,
            "spmc_ring".to_string(),
            Some(100),
            1,
            2,
            false,
            0,
        )];

        let pretty = format_records_json(&records, true).unwrap();
        assert!(pretty.contains('\n')); // Pretty has newlines

        let compact = format_records_json(&records, false).unwrap();
        assert!(!compact.contains('\n')); // Compact is one line
    }

    fn instance() -> InstanceInfo {
        InstanceInfo {
            endpoint: "/tmp/aimdb.sock".into(),
            server_version: "aimdb/2.0.0".to_string(),
            protocol_version: "2".to_string(),
            permissions: vec!["read".to_string()],
            writable_records: vec![],
            max_subscriptions: Some(8),
            authenticated: false,
        }
    }

    #[test]
    fn instances_value_names_the_endpoint_socket_path() {
        let value = instances_value(&[instance()]);
        assert_eq!(value[0]["socket_path"], "/tmp/aimdb.sock");
        assert_eq!(value[0]["server_version"], "aimdb/2.0.0");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn instances_yaml_uses_the_json_field_names() {
        let yaml = serde_yaml::to_string(&instances_value(&[instance()])).unwrap();
        assert!(yaml.contains("socket_path: /tmp/aimdb.sock"), "{yaml}");
    }
}
