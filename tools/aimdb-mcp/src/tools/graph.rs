//! Graph introspection tools (graph_nodes, graph_edges, graph_topo_order)

use crate::error::{McpError, McpResult};
use aimdb_client::AimxClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

/// Parameters for graph_nodes tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphNodesParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
}

/// Parameters for graph_edges tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphEdgesParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
}

/// Parameters for graph_topo_order tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphTopoOrderParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
}

/// Get all nodes in the dependency graph
///
/// Returns metadata for all records as graph nodes, including their
/// origin (source, link, transform, passive), buffer configuration,
/// and connection counts.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
///
/// # Returns
/// - Array of GraphNode objects with:
///   - `key`: Record key (e.g., "temp.vienna")
///   - `origin`: How the record gets its values
///   - `buffer_type`: Buffer type used
///   - `buffer_capacity`: Optional buffer capacity
///   - `tap_count`: Number of taps attached
///   - `has_outbound_link`: Whether an outbound connector is configured
pub async fn graph_nodes(args: Option<Value>) -> McpResult<Value> {
    debug!("🔗 graph_nodes called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: GraphNodesParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!("🔌 Connecting to {}", params.socket_path);

    // Get or create connection from pool (if available)
    let mut client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxClient::connect(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    };

    // Get graph nodes
    let nodes = client.graph_nodes().await.map_err(McpError::Client)?;

    debug!("✅ Retrieved {} graph nodes", nodes.len());

    // Return as JSON value
    serde_json::to_value(nodes)
        .map_err(|e| McpError::Internal(format!("JSON serialization failed: {}", e)))
}

/// Get all edges in the dependency graph
///
/// Returns all directed edges representing data flow between records.
/// Edges show how data flows from sources through transforms to consumers.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
///
/// # Returns
/// - Array of GraphEdge objects with:
///   - `from`: Source record key (None for external origins)
///   - `to`: Target record key (None for side-effects)
///   - `edge_type`: Classification of the edge
pub async fn graph_edges(args: Option<Value>) -> McpResult<Value> {
    debug!("🔗 graph_edges called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: GraphEdgesParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!("🔌 Connecting to {}", params.socket_path);

    // Get or create connection from pool (if available)
    let mut client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxClient::connect(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    };

    // Get graph edges
    let edges = client.graph_edges().await.map_err(McpError::Client)?;

    debug!("✅ Retrieved {} graph edges", edges.len());

    // Return as JSON value
    serde_json::to_value(edges)
        .map_err(|e| McpError::Internal(format!("JSON serialization failed: {}", e)))
}

/// Get the topological ordering of records
///
/// Returns record keys in topological order, ensuring all dependencies
/// are listed before their dependents. This order is used internally
/// for spawn ordering and reflects the proper initialization sequence.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
///
/// # Returns
/// - Array of record keys in topological order
pub async fn graph_topo_order(args: Option<Value>) -> McpResult<Value> {
    debug!("📊 graph_topo_order called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: GraphTopoOrderParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!("🔌 Connecting to {}", params.socket_path);

    // Get or create connection from pool (if available)
    let mut client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxClient::connect(&params.socket_path)
            .await
            .map_err(McpError::Client)?
    };

    // Get topological order
    let order = client.graph_topo_order().await.map_err(McpError::Client)?;

    debug!(
        "✅ Retrieved topological order with {} records",
        order.len()
    );

    // Return as JSON value
    serde_json::to_value(order)
        .map_err(|e| McpError::Internal(format!("JSON serialization failed: {}", e)))
}
