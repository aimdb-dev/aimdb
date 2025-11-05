//! Record-related tools (list_records, get_record, set_record)

use crate::error::{McpError, McpResult};
use aimdb_client::AimxClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

/// Parameters for list_records tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListRecordsParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
}

/// Parameters for get_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetRecordParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
    /// Name of the record to retrieve
    record_name: String,
}

/// Parameters for set_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetRecordParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
    /// Name of the record to update
    record_name: String,
    /// New value for the record (JSON)
    value: Value,
}

/// Record information (MCP format)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordInfo {
    /// Record type name
    name: String,
    /// TypeId as hexadecimal string
    type_id: String,
    /// Buffer type: "spmc_ring", "single_latest", "mailbox", or "none"
    buffer_type: String,
    /// Buffer capacity (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    buffer_capacity: Option<usize>,
    /// Number of producers
    producer_count: usize,
    /// Number of consumers
    consumer_count: usize,
    /// Whether write operations are permitted
    writable: bool,
    /// Creation timestamp (ISO 8601)
    created_at: String,
    /// Last update timestamp (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    last_update: Option<String>,
    /// Number of connector links
    connector_count: usize,
}

/// List all records from a specific AimDB instance
///
/// Connects to the specified socket and retrieves the list of all
/// registered records with their metadata.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
///
/// # Returns
/// - Array of records with metadata
pub async fn list_records(args: Option<Value>) -> McpResult<Value> {
    debug!("📋 list_records called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: ListRecordsParams = serde_json::from_value(args.unwrap_or(Value::Null))
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

    // List records
    let records = client.list_records().await.map_err(McpError::Client)?;

    debug!("✅ Found {} record(s)", records.len());

    // Convert to MCP format
    let record_infos: Vec<RecordInfo> = records
        .into_iter()
        .map(|r| RecordInfo {
            name: r.name,
            type_id: r.type_id,
            buffer_type: r.buffer_type,
            buffer_capacity: r.buffer_capacity,
            producer_count: r.producer_count,
            consumer_count: r.consumer_count,
            writable: r.writable,
            created_at: r.created_at,
            last_update: r.last_update,
            connector_count: r.connector_count,
        })
        .collect();

    // Serialize to JSON value
    serde_json::to_value(record_infos)
        .map_err(|e| McpError::Internal(format!("JSON serialization failed: {}", e)))
}

/// Get the current value of a specific record
///
/// Connects to the specified socket and retrieves the current value
/// of the named record.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
/// - `record_name` (required): Name of the record to retrieve
///
/// # Returns
/// - Current record value as JSON
pub async fn get_record(args: Option<Value>) -> McpResult<Value> {
    debug!("🔍 get_record called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: GetRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!(
        "🔌 Connecting to {} to get record '{}'",
        params.socket_path, params.record_name
    );

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

    // Get record value
    let value = client
        .get_record(&params.record_name)
        .await
        .map_err(McpError::Client)?;

    debug!("✅ Retrieved record '{}'", params.record_name);

    Ok(value)
}

/// Set the value of a writable record
///
/// Connects to the specified socket and updates the value of a writable record.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
/// - `record_name` (required): Name of the record to update
/// - `value` (required): New value for the record (JSON)
///
/// # Returns
/// - Success confirmation
pub async fn set_record(args: Option<Value>) -> McpResult<Value> {
    debug!("✏️  set_record called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: SetRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!(
        "🔌 Connecting to {} to set record '{}'",
        params.socket_path, params.record_name
    );

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

    // Set record value
    let result = client
        .set_record(&params.record_name, params.value)
        .await
        .map_err(McpError::Client)?;

    debug!("✅ Updated record '{}'", params.record_name);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_list_records_missing_socket_path() {
        // Should fail without socket_path parameter
        let result = list_records(None).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.message().contains("Invalid parameters"));
    }

    #[tokio::test]
    async fn test_list_records_invalid_socket() {
        // Should fail with non-existent socket
        let params = json!({
            "socket_path": "/tmp/nonexistent.sock"
        });

        let result = list_records(Some(params)).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(
            err.message().contains("Failed to connect") || err.message().contains("No such file")
        );
    }

    #[tokio::test]
    async fn test_get_record_missing_params() {
        // Should fail without required parameters
        let result = get_record(None).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.message().contains("Invalid parameters"));
    }

    #[tokio::test]
    async fn test_get_record_invalid_socket() {
        // Should fail with non-existent socket
        let params = json!({
            "socket_path": "/tmp/nonexistent.sock",
            "record_name": "TestRecord"
        });

        let result = get_record(Some(params)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_record_missing_params() {
        // Should fail without required parameters
        let result = set_record(None).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.message().contains("Invalid parameters"));
    }

    #[tokio::test]
    async fn test_set_record_invalid_socket() {
        // Should fail with non-existent socket
        let params = json!({
            "socket_path": "/tmp/nonexistent.sock",
            "record_name": "TestRecord",
            "value": {"test": "value"}
        });

        let result = set_record(Some(params)).await;
        assert!(result.is_err());
    }
}
