//! Instance Management Commands

use crate::client::discovery::{discover_instances, find_instance};
use crate::error::CliResult;
use crate::output::{json, table, OutputFormat};
use clap::Args;

/// Instance management commands
#[derive(Debug, Args)]
pub struct InstanceCommand {
    #[command(subcommand)]
    pub subcommand: InstanceSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum InstanceSubcommand {
    /// List all running AimDB instances
    List {
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show detailed information about an instance
    Info {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,
    },
    /// Test connection to an instance
    Ping {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,
    },
}

impl InstanceCommand {
    pub async fn execute(self) -> CliResult<()> {
        match self.subcommand {
            InstanceSubcommand::List { format } => list_instances(format).await,
            InstanceSubcommand::Info { socket } => show_instance_info(socket.as_deref()).await,
            InstanceSubcommand::Ping { socket } => ping_instance(socket.as_deref()).await,
        }
    }
}

async fn list_instances(format: OutputFormat) -> CliResult<()> {
    let instances = discover_instances().await?;

    let output = match format {
        OutputFormat::Table => table::format_instances_table(&instances),
        OutputFormat::Json => json::format_instances_json(&instances, true)?,
        OutputFormat::JsonCompact => json::format_instances_json(&instances, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&instances)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn show_instance_info(socket: Option<&str>) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let output = table::format_instance_info(&instance);
    println!("{}", output);
    Ok(())
}

async fn ping_instance(socket: Option<&str>) -> CliResult<()> {
    let instance = find_instance(socket).await?;

    println!("✅ Connection successful!");
    println!("  Socket: {}", instance.socket_path.display());
    println!("  Server: {}", instance.server_version);
    println!("  Protocol: {}", instance.protocol_version);

    Ok(())
}
