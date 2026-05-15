//! Buffer introspection metrics tools.
//!
//! `get_buffer_metrics` returns live `produced/consumed/dropped/occupancy`
//! counters for records matching a key. `reset_buffer_metrics` zeroes those
//! counters server-side (requires write permission and the `metrics` feature
//! on the server).

use crate::error::{McpError, McpResult};
use aimdb_client::AimxClient;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

#[derive(Debug, Deserialize)]
struct GetBufferMetricsParams {
    /// Unix socket path to the AimDB instance (falls back to AIMDB_SOCKET env)
    socket_path: Option<String>,
    /// Substring matched against record names (e.g. `"Temperature"`).
    record_key: String,
}

#[derive(Debug, Deserialize)]
struct ResetBufferMetricsParams {
    socket_path: Option<String>,
}

async fn connect(socket_path: &str) -> McpResult<AimxClient> {
    if let Some(pool) = super::connection_pool() {
        pool.get_connection(socket_path)
            .await
            .map_err(McpError::Client)
    } else {
        AimxClient::connect(socket_path)
            .await
            .map_err(McpError::Client)
    }
}

/// Returns live buffer metrics for records whose name contains `record_key`.
///
/// Session-agnostic: works in any phase.
pub async fn get_buffer_metrics(args: Option<Value>) -> McpResult<Value> {
    debug!("get_buffer_metrics called");
    let params: GetBufferMetricsParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("get_buffer_metrics: {e}")))?;
    let socket_path = super::resolve_socket_path(params.socket_path)?;

    let mut client = connect(&socket_path).await?;
    let raw = client.list_records().await.map_err(McpError::Client)?;

    let matching: Vec<_> = raw
        .into_iter()
        .filter(|r| r.name.contains(&params.record_key))
        .collect();

    if matching.is_empty() {
        return Ok(json!({
            "found": false,
            "record_key": params.record_key,
            "message": "No records matching this key were found in the running instance.",
        }));
    }

    Ok(json!({
        "found": true,
        "record_key": params.record_key,
        "records": serde_json::to_value(matching)?,
    }))
}

/// Resets buffer introspection counters for every record on the target instance.
pub async fn reset_buffer_metrics(args: Option<Value>) -> McpResult<Value> {
    debug!("reset_buffer_metrics called");
    let params: ResetBufferMetricsParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("reset_buffer_metrics: {e}")))?;
    let socket_path = super::resolve_socket_path(params.socket_path)?;

    let mut client = connect(&socket_path).await?;
    match client.reset_buffer_metrics().await {
        Ok(_) => Ok(json!({
            "reset": true,
            "message": "Buffer metrics counters reset on all records.",
        })),
        Err(aimdb_client::ClientError::ServerError { ref code, .. })
            if code == "method_not_found" =>
        {
            // The server was built without the `metrics` feature.
            Ok(json!({
                "reset": false,
                "message": "The target instance does not support buffer_metrics.reset (built without the `metrics` feature?).",
            }))
        }
        Err(e) => Err(McpError::Client(e)),
    }
}
