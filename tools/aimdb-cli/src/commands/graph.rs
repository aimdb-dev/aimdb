//! Graph Introspection Commands
//!
//! Commands for exploring the dependency graph of records in an AimDB instance.

use crate::commands::connect_endpoint;
use crate::error::CliResult;
use crate::output::{json, table, OutputFormat};
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
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List all edges in the dependency graph
    Edges {
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show topological order of records
    Order {
        /// Output format
        #[arg(short, long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Export graph in DOT format for visualization
    Dot {
        /// Graph name for the DOT output
        #[arg(short, long, default_value = "aimdb")]
        name: String,
    },
}

impl GraphCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        match self.subcommand {
            GraphSubcommand::Nodes { format } => list_nodes(endpoint, format).await,
            GraphSubcommand::Edges { format } => list_edges(endpoint, format).await,
            GraphSubcommand::Order { format } => show_topo_order(endpoint, format).await,
            GraphSubcommand::Dot { name } => export_dot(endpoint, &name).await,
        }
    }
}

async fn list_nodes(endpoint: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

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

async fn list_edges(endpoint: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

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

async fn show_topo_order(endpoint: Option<&str>, format: OutputFormat) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

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

async fn export_dot(endpoint: Option<&str>, name: &str) -> CliResult<()> {
    let client = connect_endpoint(endpoint).await?;

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
        let origin = table::variant_name(node.get("origin")).unwrap_or("passive");

        // Escape the key for DOT format
        let escaped_key = key.replace('"', "\\\"");
        let node_id = key.replace(['.', ':', '-'], "_");

        let color = match origin {
            "source" => "lightblue",
            "link" => "lightgreen",
            "transform" | "transform_join" => "lightyellow",
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
        let edge_type = table::variant_name(edge.get("edge_type")).unwrap_or("-");

        if let (Some(from), Some(to)) = (from, to) {
            let from_id = from.replace(['.', ':', '-'], "_");
            let to_id = to.replace(['.', ':', '-'], "_");

            let style = match edge_type {
                "transform" | "transform_join" => "style=bold, color=blue",
                "tap" => "style=dashed, color=gray",
                _ => "",
            };

            dot.push_str(&format!("  {} -> {} [{}];\n", from_id, to_id, style));
        }
    }

    dot.push_str("}\n");
    dot
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Origins and edge types with data are objects on the wire; DOT styles
    /// them by variant name.
    #[test]
    fn dot_styles_object_origins_and_edges() {
        let nodes = vec![
            serde_json::json!({"key": "flow.raw", "origin": {"link": {"protocol": "knx", "address": "knx://1/2/3"}}}),
            serde_json::json!({"key": "flow.avg", "origin": {"transform": {"input": "flow.raw"}}}),
        ];
        let edges = vec![
            serde_json::json!({"from": "flow.raw", "to": "flow.avg", "edge_type": "transform"}),
        ];
        let dot = generate_dot("g", &nodes, &edges);
        assert!(
            dot.contains("flow_raw [label=\"flow.raw\\n(link)\", fillcolor=lightgreen"),
            "{dot}"
        );
        assert!(
            dot.contains("flow_avg [label=\"flow.avg\\n(transform)\", fillcolor=lightyellow"),
            "{dot}"
        );
        assert!(
            dot.contains("flow_raw -> flow_avg [style=bold, color=blue]"),
            "{dot}"
        );
    }
}
