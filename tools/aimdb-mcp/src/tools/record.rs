//! Record-related tools (list_records, get_record, set_record)

use crate::error::{McpError, McpResult};
use aimdb_client::AimxConnection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// Parameters for list_records tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListRecordsParams {
    /// Endpoint of the AimDB instance: a `scheme://` URL or bare path (falls back to AIMDB_CONNECT env).
    endpoint: Option<String>,
}

/// Parameters for get_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetRecordParams {
    /// Endpoint of the AimDB instance: a `scheme://` URL or bare path (falls back to AIMDB_CONNECT env).
    endpoint: Option<String>,
    /// Name of the record to retrieve
    record_name: String,
}

/// Parameters for set_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetRecordParams {
    /// Endpoint of the AimDB instance: a `scheme://` URL or bare path (falls back to AIMDB_CONNECT env).
    endpoint: Option<String>,
    /// Name of the record to update
    record_name: String,
    /// New value for the record (JSON)
    value: Value,
}

/// Parameters for drain_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DrainRecordParams {
    /// Endpoint of the AimDB instance: a `scheme://` URL or bare path (falls back to AIMDB_CONNECT env).
    endpoint: Option<String>,
    /// Name of the record to drain
    record_name: String,
    /// Maximum number of values to drain (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

/// Record information (MCP format)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordInfo {
    /// Record key (use this for get_record/set_record)
    record_key: String,
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
    /// Number of outbound connector links
    outbound_connector_count: usize,
    /// Declared data-contract schema name, when the record was configured
    /// with one. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_type: Option<String>,
    /// Version of the schema named by `schema_type`. Absent when the record
    /// declares no schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_version: Option<u32>,
}

/// List all records from a specific AimDB instance
///
/// Connects to the instance and retrieves the list of all
/// registered records with their metadata.
///
/// # Parameters
/// - `endpoint` (optional): endpoint URL (`unix://PATH`, `serial://DEVICE?baud=N`) or bare path; falls back to `--connect` / `AIMDB_CONNECT`
///
/// # Returns
/// - Array of records with metadata
pub async fn list_records(args: Option<Value>) -> McpResult<Value> {
    debug!("📋 list_records called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: ListRecordsParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    let endpoint = super::resolve_endpoint(params.endpoint)?;
    debug!("🔌 Connecting to {}", endpoint);

    // Get or create connection from pool (if available)
    let client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&endpoint)
            .await
            .map_err(McpError::Client)?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxConnection::connect(&endpoint)
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
            record_key: r.record_key,
            name: r.name,
            type_id: r.type_id,
            buffer_type: r.buffer_type,
            buffer_capacity: r.buffer_capacity,
            producer_count: r.producer_count,
            consumer_count: r.consumer_count,
            writable: r.writable,
            outbound_connector_count: r.outbound_connector_count,
            schema_type: r.schema_type,
            schema_version: r.schema_version,
        })
        .collect();

    // Serialize to JSON value
    serde_json::to_value(record_infos)
        .map_err(|e| McpError::Internal(format!("JSON serialization failed: {}", e)))
}

/// Get the current value of a specific record
///
/// Connects to the instance and retrieves the current value
/// of the named record.
///
/// # Parameters
/// - `endpoint` (optional): endpoint URL (`unix://PATH`, `serial://DEVICE?baud=N`) or bare path; falls back to `--connect` / `AIMDB_CONNECT`
/// - `record_name` (required): Name of the record to retrieve
///
/// # Returns
/// - Current record value as JSON
pub async fn get_record(args: Option<Value>) -> McpResult<Value> {
    debug!("🔍 get_record called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: GetRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    let endpoint = super::resolve_endpoint(params.endpoint)?;
    debug!(
        "🔌 Connecting to {} to get record '{}'",
        endpoint, params.record_name
    );

    // `record.get` is a single non-destructive read with no drain cursor, so it
    // needs no retry: `not_found` (no such record, or no value yet) and
    // `unsupported` (a ring, which has no current value) come back at once.
    let client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&endpoint)
            .await
            .map_err(McpError::Client)?
    } else {
        AimxConnection::connect(&endpoint)
            .await
            .map_err(McpError::Client)?
    };

    let sample = client
        .get_record(&params.record_name)
        .await
        .map_err(McpError::Client)?;
    debug!("✅ Retrieved record '{}'", params.record_name);
    Ok(stamped_json(&sample))
}

/// Set the value of a writable record
///
/// Connects to the instance and updates the value of a writable record.
///
/// # Parameters
/// - `endpoint` (optional): endpoint URL (`unix://PATH`, `serial://DEVICE?baud=N`) or bare path; falls back to `--connect` / `AIMDB_CONNECT`
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

    let endpoint = super::resolve_endpoint(params.endpoint)?;
    debug!(
        "🔌 Connecting to {} to set record '{}'",
        endpoint, params.record_name
    );

    // Get or create connection from pool (if available)
    let client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&endpoint)
            .await
            .map_err(McpError::Client)?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxConnection::connect(&endpoint)
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

/// Drain all pending values from a record since the last drain call.
///
/// Returns values in chronological order. This is a destructive read —
/// drained values won't be returned again. Use this for batch analysis
/// of accumulated data.
///
/// # Parameters
/// - `endpoint` (optional): endpoint URL (`unix://PATH`, `serial://DEVICE?baud=N`) or bare path; falls back to `--connect` / `AIMDB_CONNECT`
/// - `record_name` (required): Name of the record to drain
/// - `limit` (optional): Maximum number of values to drain
///
/// # Returns
/// - Record name, values array, and count
pub async fn drain_record(args: Option<Value>) -> McpResult<Value> {
    debug!("🔄 drain_record called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: DrainRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    let endpoint = super::resolve_endpoint(params.endpoint)?;
    debug!(
        "🔌 Connecting to {} to drain record '{}'",
        endpoint, params.record_name
    );

    // Use persistent drain client so the server-side drain reader persists
    // across calls, allowing values to accumulate between drains.
    let pool = super::connection_pool()
        .ok_or_else(|| McpError::Internal("Connection pool not initialized".to_string()))?;

    let client_arc = pool
        .get_drain_client(&endpoint)
        .await
        .map_err(McpError::Client)?;

    let client = client_arc.lock().await;

    // Drain record values
    let response = match params.limit {
        Some(limit) => client
            .drain_record_with_limit(&params.record_name, limit)
            .await
            .map_err(|e| {
                // On connection error, invalidate so next call reconnects
                let socket = endpoint.clone();
                let pool = pool.clone();
                tokio::spawn(async move { pool.invalidate_drain_client(&socket).await });
                McpError::Client(e)
            })?,
        None => client
            .drain_record(&params.record_name)
            .await
            .map_err(|e| {
                let socket = endpoint.clone();
                let pool = pool.clone();
                tokio::spawn(async move { pool.invalidate_drain_client(&socket).await });
                McpError::Client(e)
            })?,
    };

    debug!(
        "✅ Drained {} values from record '{}'",
        response.count, params.record_name
    );

    Ok(json!({
        "record_name": response.record_name,
        "values": response.values.iter().map(stamped_json).collect::<Vec<_>>(),
        "count": response.count,
    }))
}

/// A value with its write stamp, as AimX 4.0 carries it: `{"value", "ts"}`,
/// `ts` in nanoseconds since the server instance started.
fn stamped_json(sample: &aimdb_client::Stamped<Value>) -> Value {
    json!({ "value": sample.value, "ts": sample.stamp().as_nanos() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `get_record` and every `drain_record` value: `{"value", "ts"}` with `ts`
    /// in integer nanoseconds (AimX 4.0).
    #[test]
    fn values_carry_their_write_stamp() {
        let sample = aimdb_client::Stamped::new(
            json!({"c": 21.5}),
            aimdb_client::WriteMetadata::from_wire_stamp(aimdb_client::Stamp::from_nanos(81)),
        );
        assert_eq!(
            stamped_json(&sample),
            json!({"value": {"c": 21.5}, "ts": 81})
        );
    }

    fn record_info(schema_type: Option<&str>, schema_version: Option<u32>) -> RecordInfo {
        RecordInfo {
            record_key: "temp.berlin".into(),
            name: "Temperature".into(),
            type_id: "0x1".into(),
            buffer_type: "single_latest".into(),
            buffer_capacity: None,
            producer_count: 1,
            consumer_count: 0,
            writable: false,
            outbound_connector_count: 0,
            schema_type: schema_type.map(str::to_string),
            schema_version,
        }
    }

    /// `list_records` carries the declared contract when there is one.
    #[test]
    fn record_info_includes_declared_schema() {
        let v = serde_json::to_value(record_info(Some("temperature"), Some(2))).unwrap();
        assert_eq!(v["schema_type"], json!("temperature"));
        assert_eq!(v["schema_version"], json!(2));
    }

    /// Records without a declared schema omit both fields.
    #[test]
    fn record_info_omits_absent_schema() {
        let v = serde_json::to_value(record_info(None, None)).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("schema_type"));
        assert!(!obj.contains_key("schema_version"));
    }

    #[tokio::test]
    async fn test_list_records_missing_endpoint() {
        // Should fail without endpoint parameter
        let result = list_records(None).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.message().contains("Invalid parameters"));
    }

    #[tokio::test]
    async fn test_list_records_invalid_socket() {
        // Should fail with non-existent socket
        let params = json!({
            "endpoint": "/tmp/nonexistent.sock"
        });

        let result = list_records(Some(params)).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(
            err.message().contains("Connection failed") || err.message().contains("No such file")
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
            "endpoint": "/tmp/nonexistent.sock",
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
            "endpoint": "/tmp/nonexistent.sock",
            "record_name": "TestRecord",
            "value": {"test": "value"}
        });

        let result = set_record(Some(params)).await;
        assert!(result.is_err());
    }
}
