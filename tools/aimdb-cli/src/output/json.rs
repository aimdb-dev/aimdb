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

/// Format instances as JSON
pub fn format_instances_json(
    instances: &[InstanceInfo],
    pretty: bool,
) -> Result<String, serde_json::Error> {
    // Convert to serializable format
    let data: Vec<_> = instances
        .iter()
        .map(|i| {
            serde_json::json!({
                "socket_path": i.socket_path.display().to_string(),
                "server_version": i.server_version,
                "protocol_version": i.protocol_version,
                "permissions": i.permissions,
                "writable_records": i.writable_records,
                "max_subscriptions": i.max_subscriptions,
                "authenticated": i.authenticated,
            })
        })
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_formatting() {
        use aimdb_core::record_id::{RecordId, StringKey};
        use core::any::TypeId;

        let records = vec![RecordMetadata::new(
            RecordId::new(0),
            StringKey::new("sensor.temperature"),
            TypeId::of::<i32>(),
            "Temperature".to_string(),
            "spmc_ring".to_string(),
            Some(100),
            1,
            2,
            false,
            "2025-11-02T00:00:00Z".to_string(),
            0,
        )];

        let pretty = format_records_json(&records, true).unwrap();
        assert!(pretty.contains('\n')); // Pretty has newlines

        let compact = format_records_json(&records, false).unwrap();
        assert!(!compact.contains('\n')); // Compact is one line
    }
}
