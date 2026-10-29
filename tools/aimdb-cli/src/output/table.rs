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
            instance.endpoint.display().to_string(),
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
        "Signal",
    ]);

    // Add rows
    for record in records {
        let writable_str = if record.writable {
            "yes".green().to_string()
        } else {
            "no".dimmed().to_string()
        };
        let signal_str = record
            .signal_stats
            .as_deref()
            .and_then(|signals| signals.split_first())
            .map(|(signal, rest)| {
                let unit = if signal.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", signal.unit)
                };
                let additional = if rest.is_empty() {
                    String::new()
                } else {
                    format!(" +{}", rest.len())
                };

                format!(
                    "{}{} ({}..{}, avg {}){}",
                    signal.last, unit, signal.min, signal.max, signal.mean, additional
                )
            })
            .unwrap_or_else(|| "-".dimmed().to_string());

        builder.push_record(vec![
            record.name.clone(),
            record.type_id.clone(),
            record.buffer_type.clone(),
            record.producer_count.to_string(),
            record.consumer_count.to_string(),
            writable_str,
            signal_str,
        ]);
    }

    builder.build().with(Style::modern()).to_string()
}

/// Format instance details
pub fn format_instance_info(instance: &InstanceInfo) -> String {
    let mut output = String::new();

    output.push_str(&format!("{}\n", "Instance Information".bold()));
    output.push_str(&format!("  Endpoint: {}\n", instance.endpoint.display()));
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

/// The variant name of a serialized graph enum (`origin`, `edge_type`): a
/// unit variant is a string (`"source"`), one with data an object with a
/// single key (`{"link": {...}}`).
pub fn variant_name(value: Option<&serde_json::Value>) -> Option<&str> {
    match value? {
        serde_json::Value::String(name) => Some(name),
        serde_json::Value::Object(map) if map.len() == 1 => map.keys().next().map(String::as_str),
        _ => None,
    }
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
        "Schema",
        "Buffer Type",
        "Capacity",
        "Taps",
        "Outbound",
    ]);

    // Add rows
    for node in nodes {
        let key = node.get("key").and_then(|v| v.as_str()).unwrap_or("-");
        let origin = variant_name(node.get("origin")).unwrap_or("-");
        let schema = node
            .get("schema")
            .and_then(|s| Some((s.get("name")?.as_str()?, s.get("version")?.as_u64()?)))
            .map(|(name, version)| format!("{name} v{version}"))
            .unwrap_or_else(|| "-".to_string());
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
        let outbound_links: Vec<&str> = node
            .get("outbound_links")
            .and_then(|v| v.as_array())
            .map(|links| links.iter().filter_map(|l| l.as_str()).collect())
            .unwrap_or_default();
        // Servers before #332 send only the flag.
        let has_outbound = node
            .get("has_outbound_link")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let origin_colored = match origin {
            "source" => origin.cyan().to_string(),
            "link" => origin.green().to_string(),
            "transform" | "transform_join" => origin.yellow().to_string(),
            _ => origin.dimmed().to_string(),
        };

        let outbound_str = if !outbound_links.is_empty() {
            outbound_links.join("\n").green().to_string()
        } else if has_outbound {
            "yes".green().to_string()
        } else {
            "-".dimmed().to_string()
        };

        builder.push_record(vec![
            key.to_string(),
            origin_colored,
            schema,
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
        let edge_type = variant_name(edge.get("edge_type")).unwrap_or("-");

        let edge_type_colored = match edge_type {
            "transform" | "transform_join" => edge_type.blue().bold().to_string(),
            "tap" => edge_type.dimmed().to_string(),
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
        use aimdb_core::profiling::SignalStatsInfo;
        use aimdb_core::record_id::{RecordId, StringKey};
        use core::any::TypeId;

        colored::control::set_override(false);

        let mut temperature = RecordMetadata::new(
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
        )
        .with_signal_stats(vec![
            SignalStatsInfo {
                signal: "temperature".to_string(),
                unit: "C".to_string(),
                count: 4,
                last: 21.5,
                min: 18.0,
                max: 24.0,
                mean: 20.25,
            },
            SignalStatsInfo {
                signal: "humidity".to_string(),
                unit: "%".to_string(),
                count: 4,
                last: 40.0,
                min: 35.0,
                max: 42.0,
                mean: 39.0,
            },
        ]);
        temperature.type_id = "type-temperature".to_string();

        let mut config = RecordMetadata::new(
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
            1,
        );
        config.type_id = "type-config".to_string();

        let table = format_records_table(&[temperature, config]);
        assert_eq!(
            table,
            "\
┌─────────────┬──────────────────┬─────────────┬───────────┬───────────┬──────────┬───────────────────────────────┐
│ Name        │ Type ID          │ Buffer Type │ Producers │ Consumers │ Writable │ Signal                        │
├─────────────┼──────────────────┼─────────────┼───────────┼───────────┼──────────┼───────────────────────────────┤
│ Temperature │ type-temperature │ spmc_ring   │ 1         │ 2         │ no       │ 21.5 C (18..24, avg 20.25) +1 │
├─────────────┼──────────────────┼─────────────┼───────────┼───────────┼──────────┼───────────────────────────────┤
│ Config      │ type-config      │ mailbox     │ 0         │ 3         │ yes      │ -                             │
└─────────────┴──────────────────┴─────────────┴───────────┴───────────┴──────────┴───────────────────────────────┘"
        );
    }

    #[test]
    fn test_format_records_table_signal_without_unit() {
        use aimdb_core::graph::RecordOrigin;
        use aimdb_core::profiling::SignalStatsInfo;
        use aimdb_core::record_id::{RecordId, StringKey};
        use core::any::TypeId;

        colored::control::set_override(false);

        let mut record = RecordMetadata::new(
            RecordId::new(0),
            StringKey::new("system.load"),
            TypeId::of::<f64>(),
            "Load".to_string(),
            RecordOrigin::Source,
            "single_latest".to_string(),
            Some(1),
            1,
            0,
            false,
            0,
        )
        .with_signal_stats(vec![SignalStatsInfo {
            signal: "load".to_string(),
            unit: String::new(),
            count: 3,
            last: 0.75,
            min: 0.5,
            max: 1.0,
            mean: 0.625,
        }]);
        record.type_id = "type-load".to_string();

        let table = format_records_table(&[record]);
        assert_eq!(
            table,
            "\
┌──────┬───────────┬───────────────┬───────────┬───────────┬──────────┬──────────────────────────┐
│ Name │ Type ID   │ Buffer Type   │ Producers │ Consumers │ Writable │ Signal                   │
├──────┼───────────┼───────────────┼───────────┼───────────┼──────────┼──────────────────────────┤
│ Load │ type-load │ single_latest │ 1         │ 0         │ no       │ 0.75 (0.5..1, avg 0.625) │
└──────┴───────────┴───────────────┴───────────┴───────────┴──────────┴──────────────────────────┘"
        );
    }

    /// #332: schema and outbound addresses are shown; origins and edge types
    /// with data (objects on the wire) show their variant name.
    #[test]
    fn graph_tables_show_schema_links_and_object_variants() {
        colored::control::set_override(false);
        let nodes = vec![
            serde_json::json!({
                "key": "flow.avg",
                "origin": {"transform": {"input": "flow.raw"}},
                "buffer_type": "single_latest",
                "buffer_capacity": null,
                "tap_count": 0,
                "has_outbound_link": true,
                "outbound_links": ["mqtt://plant/boiler/flow_avg"],
                "schema": {"name": "flow", "version": 2},
            }),
            // A server before #332: no `schema`, no `outbound_links`.
            serde_json::json!({
                "key": "flow.raw",
                "origin": {"link": {"protocol": "knx", "address": "knx://1/2/3"}},
                "buffer_type": "spmc_ring",
                "buffer_capacity": 16,
                "tap_count": 1,
                "has_outbound_link": true,
            }),
        ];
        let table = format_graph_nodes_table(&nodes);
        assert!(table.contains("transform"), "{table}");
        assert!(table.contains("link"), "{table}");
        assert!(table.contains("flow v2"), "{table}");
        assert!(table.contains("mqtt://plant/boiler/flow_avg"), "{table}");
        assert!(table.contains("yes"), "{table}");

        let edges = vec![
            serde_json::json!({"from": "flow.raw", "to": "flow.avg", "edge_type": "transform"}),
            serde_json::json!({"from": "flow.avg", "to": null,
                "edge_type": {"link_out": {"protocol": "mqtt"}}}),
        ];
        let table = format_graph_edges_table(&edges);
        assert!(table.contains("link_out"), "{table}");
        assert!(table.contains("transform"), "{table}");
    }
}
