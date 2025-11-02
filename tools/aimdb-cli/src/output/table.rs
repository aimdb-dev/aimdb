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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_records_table() {
        use core::any::TypeId;

        let records = vec![
            RecordMetadata::new(
                TypeId::of::<i32>(),
                "Temperature".to_string(),
                "spmc_ring".to_string(),
                Some(100),
                1,
                2,
                false,
                "2025-11-02T00:00:00Z".to_string(),
                0,
            ),
            RecordMetadata::new(
                TypeId::of::<String>(),
                "Config".to_string(),
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
