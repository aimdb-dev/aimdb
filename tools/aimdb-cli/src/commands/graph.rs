//! Graph Introspection Commands
//!
//! Commands for exploring the dependency graph of records in an AimDB instance.

use crate::error::CliResult;
use crate::output::{json, table, OutputFormat};
use aimdb_client::connection::AimxClient;
use aimdb_client::discovery::find_instance;
use clap::Args;
use serde_json::Value;

/// Graph introspection commands
#[derive(Debug, Args)]
pub struct GraphCommand {
    #[command(subcommand)]
    pub subcommand: GraphSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum GraphSubcommand {
    /// List all nodes in the dependency graph
    Nodes {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,

        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List all edges in the dependency graph
    Edges {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,

        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show topological order of records
    Order {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,

        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Export graph in DOT format for visualization
    Dot {
        /// Socket path (optional, uses auto-discovery if not specified)
        #[arg(short, long)]
        socket: Option<String>,

        /// Graph name for the DOT output
        #[arg(short, long, default_value = "aimdb")]
        name: String,
    },
}

impl GraphCommand {
    pub async fn execute(self) -> CliResult<()> {
        match self.subcommand {
            GraphSubcommand::Nodes { socket, format } => {
                list_nodes(socket.as_deref(), format).await
            }
            GraphSubcommand::Edges { socket, format } => {
                list_edges(socket.as_deref(), format).await
            }
            GraphSubcommand::Order { socket, format } => {
                show_topo_order(socket.as_deref(), format).await
            }
            GraphSubcommand::Dot { socket, name } => export_dot(socket.as_deref(), &name).await,
        }
    }
}

async fn list_nodes(socket: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let mut client = AimxClient::connect(&instance.socket_path).await?;

    let nodes = client.graph_nodes().await?;

    let output = match format {
        OutputFormat::Table => table::format_graph_nodes_table(&nodes),
        OutputFormat::Json => json::format_json(&nodes, true)?,
        OutputFormat::JsonCompact => json::format_json(&nodes, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&nodes)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn list_edges(socket: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let mut client = AimxClient::connect(&instance.socket_path).await?;

    let edges = client.graph_edges().await?;

    let output = match format {
        OutputFormat::Table => table::format_graph_edges_table(&edges),
        OutputFormat::Json => json::format_json(&edges, true)?,
        OutputFormat::JsonCompact => json::format_json(&edges, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&edges)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn show_topo_order(socket: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let mut client = AimxClient::connect(&instance.socket_path).await?;

    let order = client.graph_topo_order().await?;

    let output = match format {
        OutputFormat::Table => table::format_topo_order_table(&order),
        OutputFormat::Json => json::format_json(&order, true)?,
        OutputFormat::JsonCompact => json::format_json(&order, false)?,
        #[cfg(feature = "yaml")]
        OutputFormat::Yaml => serde_yaml::to_string(&order)
            .map_err(|e| anyhow::anyhow!("YAML serialization error: {}", e))?,
    };

    println!("{}", output);
    Ok(())
}

async fn export_dot(socket: Option<&str>, name: &str) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let mut client = AimxClient::connect(&instance.socket_path).await?;

    let nodes = client.graph_nodes().await?;
    let edges = client.graph_edges().await?;

    let dot = generate_dot(name, &nodes, &edges);
    println!("{}", dot);
    Ok(())
}

/// Generate DOT format graph from nodes and edges
fn generate_dot(name: &str, nodes: &[Value], edges: &[Value]) -> String {
    let mut dot = String::new();

    dot.push_str(&format!("digraph {} {{\n", name));
    dot.push_str("  rankdir=TB;\n");
    dot.push_str("  node [shape=box, style=rounded];\n");
    dot.push('\n');

    // Add nodes with styling based on origin
    for node in nodes {
        let key = node
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let origin = node
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("passive");

        // Escape the key for DOT format
        let escaped_key = key.replace('"', "\\\"");
        let node_id = key.replace(['.', ':', '-'], "_");

        let color = match origin {
            "source" => "lightblue",
            "link" => "lightgreen",
            "transform" => "lightyellow",
            _ => "lightgray",
        };

        dot.push_str(&format!(
            "  {} [label=\"{}\\n({})\", fillcolor={}, style=\"rounded,filled\"];\n",
            node_id, escaped_key, origin, color
        ));
    }

    dot.push('\n');

    // Add edges
    for edge in edges {
        let from = edge.get("from").and_then(|v| v.as_str());
        let to = edge.get("to").and_then(|v| v.as_str());
        let edge_type = edge
            .get("edge_type")
            .and_then(|v| v.as_str())
            .unwrap_or("data_flow");

        if let (Some(from), Some(to)) = (from, to) {
            let from_id = from.replace(['.', ':', '-'], "_");
            let to_id = to.replace(['.', ':', '-'], "_");

            let style = match edge_type {
                "transform_input" => "style=bold, color=blue",
                "tap" => "style=dashed, color=gray",
                _ => "",
            };

            dot.push_str(&format!("  {} -> {} [{}];\n", from_id, to_id, style));
        }
    }

    dot.push_str("}\n");
    dot
}
