//! Record Management Commands

use crate::commands::connect_endpoint;
use crate::error::CliResult;
use crate::output::{json, table, OutputFormat};
use clap::Args;

/// Record management commands
#[derive(Debug, Args)]
pub struct RecordCommand {
    #[command(subcommand)]
    pub subcommand: RecordSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum RecordSubcommand {
    /// List all registered records
    List {
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,

        /// Show only writable records
        #[arg(short, long)]
        writable: bool,
    },
    /// Get current value of a record
    Get {
        /// Record name
        record: String,

        /// Output format
        #[arg(short, long, value_enum, default_value = "json")]
        format: OutputFormat,
    },
    /// Set value of a writable record
    Set {
        /// Record name
        name: String,

        /// JSON value to set
        value: String,

        /// Dry run - validate but don't actually set
        #[arg(long)]
        dry_run: bool,
    },
}

impl RecordCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        match self.subcommand {
            RecordSubcommand::List { format, writable } => {
                list_records(endpoint, format, writable).await
            }
            RecordSubcommand::Get { record, format } => get_record(&record, endpoint, format).await,
            RecordSubcommand::Set {
                name,
                value,
                dry_run,
            } => set_record(&name, &value, endpoint, dry_run).await,
        }
    }
}

async fn list_records(
    endpoint: Option<&str>,
    format: OutputFormat,
    writable_only: bool,
) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

    let mut records = client.list_records().await?;

    // Filter to writable only if requested
    if writable_only {
        records.retain(|r| r.writable);
    }

    let output = match format {
        OutputFormat::Table => table::format_records_table(&records),
        OutputFormat::Json => json::format_records_json(&records, true)?,
        OutputFormat::JsonCompact => json::format_records_json(&records, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&records)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn get_record(name: &str, endpoint: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

    let value = client.get_record(name).await?;

    let output = match format {
        OutputFormat::Table => {
            // For single record, just show pretty JSON
            serde_json::to_string_pretty(&value)?
        }
        OutputFormat::Json => serde_json::to_string_pretty(&value)?,
        OutputFormat::JsonCompact => serde_json::to_string(&value)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&value)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn set_record(
    name: &str,
    value_str: &str,
    endpoint: Option<&str>,
    dry_run: bool,
) -> CliResult<()> {
    // Parse JSON value
    let value: serde_json::Value = serde_json::from_str(value_str)
        .map_err(|e| crate::error::CliError::invalid_json(value_str, e))?;

    if dry_run {
        println!("🔍 Dry run mode - would set:");
        println!("  Record: {}", name);
        println!("  Value: {}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let client = connect_endpoint(endpoint).await?;

    let result = client.set_record(name, value).await?;

    println!("✅ Successfully set record: {}", name);
    println!();
    println!("Result:");
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}
