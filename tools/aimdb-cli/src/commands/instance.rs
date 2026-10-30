//! Instance Management Commands

use crate::error::CliResult;
use crate::output::{json, table, OutputFormat};
use aimdb_client::discovery::{discover_instances, find_instance, InstanceInfo};
use aimdb_client::AimxConnection;
use clap::Args;
use std::path::PathBuf;

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
    /// Show detailed information about an instance: identity, build info
    /// (protocol 3.1), permissions
    Info {
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
        /// Also read the post-link region's bytes, shown as hex
        #[arg(long)]
        post_link: bool,
    },
    /// Test connection to an instance
    Ping {},
}

impl InstanceCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        match self.subcommand {
            // `list` is discovery-only (a Unix-socket scan); it ignores --connect.
            InstanceSubcommand::List { format } => list_instances(format).await,
            InstanceSubcommand::Info { format, post_link } => {
                show_instance_info(endpoint, format, post_link).await
            }
            InstanceSubcommand::Ping {} => ping_instance(endpoint).await,
        }
    }
}

/// Resolve an instance's info: an explicit endpoint connects and reads the
/// server's Welcome; `None` falls back to UDS auto-discovery.
async fn resolve_instance(endpoint: Option<&str>) -> CliResult<InstanceInfo> {
    match endpoint {
        Some(ep) => {
            let conn = AimxConnection::connect(ep).await?;
            Ok(InstanceInfo::from((
                PathBuf::from(ep),
                conn.server_info().clone(),
            )))
        }
        None => Ok(find_instance(None).await?),
    }
}

async fn list_instances(format: OutputFormat) -> CliResult<()> {
    let instances = discover_instances().await?;

    let output = match format {
        OutputFormat::Table => table::format_instances_table(&instances),
        OutputFormat::Json => json::format_instances_json(&instances, true)?,
        OutputFormat::JsonCompact => json::format_instances_json(&instances, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&json::instances_value(&instances))
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn show_instance_info(
    endpoint: Option<&str>,
    format: OutputFormat,
    with_post_link: bool,
) -> CliResult<()> {
    // `instance.info` needs a connection, so discovery only picks the endpoint.
    let endpoint = match endpoint {
        Some(ep) => ep.to_string(),
        None => find_instance(None).await?.endpoint.display().to_string(),
    };
    let conn = AimxConnection::connect(&endpoint).await?;
    let instance = InstanceInfo::from((PathBuf::from(&endpoint), conn.server_info().clone()));
    let reply = conn.instance_info().await?;
    let post_link = match with_post_link {
        true => Some(conn.post_link().await?),
        false => None,
    };
    let post_link = post_link.as_ref().map(Option::as_deref);

    let value = || json::instance_info_value(&instance, reply.as_ref(), post_link);
    let output = match format {
        OutputFormat::Table => table::format_instance_info(&instance, reply.as_ref(), post_link),
        OutputFormat::Json => json::format_json_pretty(&value())?,
        OutputFormat::JsonCompact => json::format_json_compact(&value())?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&value())
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };
    println!("{}", output);
    Ok(())
}

async fn ping_instance(endpoint: Option<&str>) -> CliResult<()> {
    let instance = resolve_instance(endpoint).await?;

    println!("✅ Connection successful!");
    println!("  Endpoint: {}", instance.endpoint.display());
    println!("  Server: {}", instance.server_version);
    println!("  Protocol: {}", instance.protocol_version);

    Ok(())
}
