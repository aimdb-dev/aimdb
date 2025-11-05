//! Schema query tool - infers JSON Schema from record values

use crate::error::{McpError, McpResult};
use aimdb_client::AimxClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// Parameters for query_schema tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySchemaParams {
    /// Unix socket path to the AimDB instance
    pub socket_path: String,

    /// Name of the record to query schema for
    pub record_name: String,

    /// Include current value as example (default: true)
    #[serde(default = "default_include_example")]
    pub include_example: bool,
}

fn default_include_example() -> bool {
    true
}

/// Infer JSON Schema from a JSON value
///
/// Performs a single-pass traversal of the JSON structure to generate
/// a JSON Schema representation. Assumes homogeneous arrays (checks first element only).
///
/// # Arguments
/// * `value` - The JSON value to analyze
///
/// # Returns
/// * JSON Schema representation of the value's structure
fn infer_json_schema(value: &Value) -> McpResult<Value> {
    match value {
        Value::Null => Ok(json!({"type": "null"})),

        Value::Bool(_) => Ok(json!({"type": "boolean"})),

        Value::Number(n) => {
            // Simple distinction: i64/u64 = integer, f64 = number
            if n.is_i64() || n.is_u64() {
                Ok(json!({"type": "integer"}))
            } else {
                Ok(json!({"type": "number"}))
            }
        }

        Value::String(_) => Ok(json!({"type": "string"})),

        Value::Array(arr) => {
            if arr.is_empty() {
                Ok(json!({
                    "type": "array",
                    "items": {},
                    "description": "Empty array - item type unknown"
                }))
            } else {
                // Infer schema from first element (assume homogeneous)
                let item_schema = infer_json_schema(&arr[0])?;
                Ok(json!({
                    "type": "array",
                    "items": item_schema
                }))
            }
        }

        Value::Object(obj) => {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();

            for (key, val) in obj.iter() {
                properties.insert(key.clone(), infer_json_schema(val)?);
                required.push(key.clone());
            }

            Ok(json!({
                "type": "object",
                "properties": properties,
                "required": required
            }))
        }
    }
}

/// Query schema for a record
///
/// Connects to an AimDB instance and infers the JSON Schema for a specific record
/// by analyzing its current value. Returns the schema along with record metadata.
///
/// # Parameters
/// - `socket_path` (required): Unix socket path to the AimDB instance
/// - `record_name` (required): Name of the record to query
/// - `include_example` (optional): Include current value as example (default: true)
///
/// # Returns
/// JSON object containing:
/// - `record_name`: Name of the queried record
/// - `schema`: JSON Schema representation
/// - `metadata`: Record metadata (buffer type, capacity, counts, timestamps)
/// - `example`: Current value (if include_example is true)
/// - `inferred_at`: Timestamp when schema was inferred
pub async fn query_schema(args: Option<Value>) -> McpResult<Value> {
    debug!("🔍 query_schema called with args: {:?}", args.as_ref());

    // Parse parameters
    let params: QuerySchemaParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!(
        "📊 Querying schema for record '{}' at {}",
        params.record_name, params.socket_path
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

    // Fetch record metadata
    let metadata_list = client.list_records().await.map_err(McpError::Client)?;

    let metadata = metadata_list
        .iter()
        .find(|m| m.name == params.record_name)
        .ok_or_else(|| {
            let available: Vec<_> = metadata_list.iter().map(|m| m.name.as_str()).collect();
            McpError::InvalidParams(format!(
                "Record '{}' not found. Available records: {}",
                params.record_name,
                available.join(", ")
            ))
        })?;

    debug!("✅ Found record metadata: {:?}", metadata.name);

    // Fetch current value (for schema inference)
    let value = client
        .get_record(&params.record_name)
        .await
        .map_err(McpError::Client)?;

    debug!("📦 Retrieved record value, inferring schema...");

    // Infer JSON schema from value
    let schema = infer_json_schema(&value)?;

    debug!("✨ Schema inference complete");

    // Build response
    let mut response = json!({
        "record_name": params.record_name,
        "schema": schema,
        "metadata": {
            "type_id": metadata.type_id,
            "buffer_type": metadata.buffer_type,
            "buffer_capacity": metadata.buffer_capacity,
            "writable": metadata.writable,
            "producer_count": metadata.producer_count,
            "consumer_count": metadata.consumer_count,
            "created_at": metadata.created_at,
            "last_update": metadata.last_update,
            "connector_count": metadata.connector_count,
        },
        "inferred_at": chrono::Utc::now().to_rfc3339_opts(
            chrono::SecondsFormat::Nanos,
            true
        ),
    });

    if params.include_example {
        response["example"] = value;
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_primitive_types() {
        // Null
        assert_eq!(
            infer_json_schema(&json!(null)).unwrap(),
            json!({"type": "null"})
        );

        // Boolean
        assert_eq!(
            infer_json_schema(&json!(true)).unwrap(),
            json!({"type": "boolean"})
        );
        assert_eq!(
            infer_json_schema(&json!(false)).unwrap(),
            json!({"type": "boolean"})
        );

        // Integer
        assert_eq!(
            infer_json_schema(&json!(42)).unwrap(),
            json!({"type": "integer"})
        );
        assert_eq!(
            infer_json_schema(&json!(-100)).unwrap(),
            json!({"type": "integer"})
        );

        // Number (float)
        assert_eq!(
            infer_json_schema(&json!(3.14)).unwrap(),
            json!({"type": "number"})
        );
        assert_eq!(
            infer_json_schema(&json!(-2.5)).unwrap(),
            json!({"type": "number"})
        );

        // String
        assert_eq!(
            infer_json_schema(&json!("hello")).unwrap(),
            json!({"type": "string"})
        );
    }

    #[test]
    fn test_infer_object_schema() {
        let value = json!({
            "name": "Alice",
            "age": 30,
            "active": true
        });

        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].is_object());
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["age"]["type"], "integer");
        assert_eq!(schema["properties"]["active"]["type"], "boolean");

        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 3);
        assert!(required.contains(&json!("name")));
        assert!(required.contains(&json!("age")));
        assert!(required.contains(&json!("active")));
    }

    #[test]
    fn test_infer_array_schema() {
        // Array with elements
        let value = json!([1, 2, 3]);
        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "integer");

        // Array of strings
        let value = json!(["a", "b", "c"]);
        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "string");
    }

    #[test]
    fn test_infer_empty_array() {
        let value = json!([]);
        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "array");
        assert!(schema["items"].is_object());
        assert!(schema["items"].as_object().unwrap().is_empty());
        assert!(schema["description"]
            .as_str()
            .unwrap()
            .contains("Empty array"));
    }

    #[test]
    fn test_infer_nested_schema() {
        let value = json!({
            "sensor": {
                "id": "sensor-01",
                "location": "Room A"
            },
            "reading": 42.5,
            "timestamp": 1730649600
        });

        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["sensor"]["type"], "object");
        assert_eq!(
            schema["properties"]["sensor"]["properties"]["id"]["type"],
            "string"
        );
        assert_eq!(
            schema["properties"]["sensor"]["properties"]["location"]["type"],
            "string"
        );
        assert_eq!(schema["properties"]["reading"]["type"], "number");
        assert_eq!(schema["properties"]["timestamp"]["type"], "integer");
    }

    #[test]
    fn test_infer_array_of_objects() {
        let value = json!([
            {"name": "Alice", "age": 30},
            {"name": "Bob", "age": 25}
        ]);

        let schema = infer_json_schema(&value).unwrap();

        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "object");
        assert_eq!(schema["items"]["properties"]["name"]["type"], "string");
        assert_eq!(schema["items"]["properties"]["age"]["type"], "integer");
    }

    #[test]
    fn test_query_schema_params_defaults() {
        let params: QuerySchemaParams = serde_json::from_value(json!({
            "socket_path": "/tmp/test.sock",
            "record_name": "test::Record"
        }))
        .unwrap();

        assert_eq!(params.socket_path, "/tmp/test.sock");
        assert_eq!(params.record_name, "test::Record");
        assert!(params.include_example); // Should default to true
    }

    #[test]
    fn test_query_schema_params_explicit_example() {
        let params: QuerySchemaParams = serde_json::from_value(json!({
            "socket_path": "/tmp/test.sock",
            "record_name": "test::Record",
            "include_example": false
        }))
        .unwrap();

        assert!(!params.include_example);
    }
}
