//! Table Output Formatting

use aimdb_client::discovery::InstanceInfo;
use aimdb_client::protocol::RecordMetadata;
use colored::Colorize;
use tabled::{builder::Builder, settings::Style};

/// Format instances as a table
pub fn format_instances_table(instances: &[InstanceInfo]) -> String {
    if instances.is_empty() {
        return "No instances found".to_string();
    }

    let mut builder = Builder::default();

    // Add header
    builder.push_record(vec![
        "Socket Path",
        "Server Version",
        "Protocol",
        "Records",
        "Writable",
        "Authenticated",
    ]);

    // Add rows
    for instance in instances {
        builder.push_record(vec![
            instance.socket_path.display().to_string(),
            instance.server_version.clone(),
            instance.protocol_version.clone(),
            instance.permissions.len().to_string(),
            instance.writable_records.len().to_string(),
            if instance.authenticated { "yes" } else { "no" }.to_string(),
        ]);
    }

    builder.build().with(Style::modern()).to_string()
}

/// Format records as a table
pub fn format_records_table(records: &[RecordMetadata]) -> String {
    if records.is_empty() {
        return "No records found".to_string();
    }

    let mut builder = Builder::default();

    // Add header
    builder.push_record(vec![
        "Name",
        "Type ID",
        "Buffer Type",
        "Producers",
        "Consumers",
        "Writable",
    ]);

    // Add rows
    for record in records {
        let writable_str = if record.writable {
            "yes".green().to_string()
        } else {
            "no".dimmed().to_string()
        };

        builder.push_record(vec![
            record.name.clone(),
            record.type_id.clone(),
            record.buffer_type.clone(),
            record.producer_count.to_string(),
            record.consumer_count.to_string(),
            writable_str,
        ]);
    }

    builder.build().with(Style::modern()).to_string()
}

/// Format instance details
pub fn format_instance_info(instance: &InstanceInfo) -> String {
    let mut output = String::new();

    output.push_str(&format!("{}\n", "Instance Information".bold()));
    output.push_str(&format!("  Socket: {}\n", instance.socket_path.display()));
    output.push_str(&format!("  Server: {}\n", instance.server_version));
    output.push_str(&format!("  Protocol: {}\n", instance.protocol_version));
    output.push_str(&format!(
        "  Authenticated: {}\n",
        if instance.authenticated { "yes" } else { "no" }
    ));

    if let Some(max_subs) = instance.max_subscriptions {
        output.push_str(&format!("  Max Subscriptions: {}\n", max_subs));
    }

    output.push_str(&format!("\n{}\n", "Permissions:".bold()));
    for perm in &instance.permissions {
        output.push_str(&format!("  - {}\n", perm));
    }

    if !instance.writable_records.is_empty() {
        output.push_str(&format!("\n{}\n", "Writable Records:".bold()));
        for record in &instance.writable_records {
            output.push_str(&format!("  - {}\n", record));
        }
    }

    output
}

/// Format graph nodes as a table
pub fn format_graph_nodes_table(nodes: &[serde_json::Value]) -> String {
    if nodes.is_empty() {
        return "No graph nodes found".to_string();
    }

    let mut builder = Builder::default();

    // Add header
    builder.push_record(vec![
        "Key",
        "Origin",
        "Buffer Type",
        "Capacity",
        "Taps",
        "Outbound",
    ]);

    // Add rows
    for node in nodes {
        let key = node.get("key").and_then(|v| v.as_str()).unwrap_or("-");
        let origin = node.get("origin").and_then(|v| v.as_str()).unwrap_or("-");
        let buffer_type = node
            .get("buffer_type")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let capacity = node
            .get("buffer_capacity")
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());
        let tap_count = node.get("tap_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let has_outbound = node
            .get("has_outbound_link")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let origin_colored = match origin {
            "source" => origin.cyan().to_string(),
            "link" => origin.green().to_string(),
            "transform" => origin.yellow().to_string(),
            _ => origin.dimmed().to_string(),
        };

        let outbound_str = if has_outbound {
            "yes".green().to_string()
        } else {
            "-".dimmed().to_string()
        };

        builder.push_record(vec![
            key.to_string(),
            origin_colored,
            buffer_type.to_string(),
            capacity,
            tap_count.to_string(),
            outbound_str,
        ]);
    }

    builder.build().with(Style::modern()).to_string()
}

/// Format graph edges as a table
pub fn format_graph_edges_table(edges: &[serde_json::Value]) -> String {
    if edges.is_empty() {
        return "No graph edges found".to_string();
    }

    let mut builder = Builder::default();

    // Add header
    builder.push_record(vec!["From", "To", "Edge Type"]);

    // Add rows
    for edge in edges {
        let from = edge
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("(external)");
        let to = edge
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("(side-effect)");
        let edge_type = edge
            .get("edge_type")
            .and_then(|v| v.as_str())
            .unwrap_or("-");

        let edge_type_colored = match edge_type {
            "transform_input" => edge_type.blue().bold().to_string(),
            "tap" => edge_type.dimmed().to_string(),
            "data_flow" => edge_type.to_string(),
            _ => edge_type.to_string(),
        };

        builder.push_record(vec![from.to_string(), to.to_string(), edge_type_colored]);
    }

    builder.build().with(Style::modern()).to_string()
}

/// Format topological order as a table
pub fn format_topo_order_table(order: &[String]) -> String {
    if order.is_empty() {
        return "No records in topological order".to_string();
    }

    let mut builder = Builder::default();

    // Add header
    builder.push_record(vec!["#", "Record Key"]);

    // Add rows with index
    for (idx, key) in order.iter().enumerate() {
        builder.push_record(vec![(idx + 1).to_string(), key.clone()]);
    }

    builder.build().with(Style::modern()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_records_table() {
        use aimdb_core::graph::RecordOrigin;
        use aimdb_core::record_id::{RecordId, StringKey};
        use core::any::TypeId;

        let records = vec![
            RecordMetadata::new(
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
                "2025-11-02T00:00:00Z".to_string(),
                0,
            ),
            RecordMetadata::new(
                RecordId::new(1),
                StringKey::new("app.config"),
                TypeId::of::<String>(),
                "Config".to_string(),
                RecordOrigin::Passive,
                "mailbox".to_string(),
                Some(1),
                0,
                3,
                true,
                "2025-11-02T00:00:00Z".to_string(),
                1,
            ),
        ];

        let table = format_records_table(&records);
        assert!(table.contains("Temperature"));
        assert!(table.contains("Config"));
    }
}
