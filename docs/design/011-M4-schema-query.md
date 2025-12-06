# Query Schema Tool - Design Document

**Version:** 1.0  
**Status:** ✅ Implemented  
**Created:** November 5, 2025  
**Related Issue:** #43 (Phase 4 - Schema Query)  
**Parent Doc:** [009-M4-mcp-integration.md](./009-M4-mcp-integration.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Problem Statement](#problem-statement)
- [Architecture Overview](#architecture-overview)
- [Schema Inference Algorithm](#schema-inference-algorithm)
- [Implementation Details](#implementation-details)
- [Integration with MCP Server](#integration-with-mcp-server)
- [Testing Strategy](#testing-strategy)
- [Future Enhancements](#future-enhancements)

---

## Executive Summary

This document describes the implementation of the `query_schema` MCP tool, which enables LLMs to understand record data types and structure without trial-and-error.

**Key Features:**
- JSON Schema inference from current record values
- Pure type inference (no heuristics or pattern matching)
- Metadata integration (buffer type, capacity, producer/consumer counts)
- Optional example value inclusion
- Performance-focused: simple, fast inference

**No core changes required** - works with existing AimDB APIs.

### Key Design Decisions

1. **Value-Based Inference**: Analyze current record values via `record.get`
2. **JSON Schema Output**: Standard format for LLM compatibility
3. **No Schema Storage**: Infer on-demand (keeps core simple)
4. **Pure Type Inference**: No pattern matching - let LLMs ask users for clarification
5. **Metadata Enrichment**: Combine type info with operational metadata
6. **Performance First**: Simple, non-recursive implementation

### Simplified Edge Cases

AimDB's architecture eliminates most edge cases:

1. **✅ Schemas are static** - Rust types don't change at runtime
2. **✅ Writable records have values** - Client API requires initialization
3. **❌ No caching needed** - Keep implementation simple
4. **❌ No pattern matching** - LLMs can ask users or check source code for units/formats

---

## Problem Statement

### Current Limitations

**Without `query_schema` tool:**
- ❌ LLMs must guess record data types through trial-and-error
- ❌ Type validation happens at runtime (inefficient, frustrating UX)
- ❌ Complex nested structures are opaque
- ❌ No way to discover field names and requirements
- ❌ Setting records requires multiple round-trips

### Use Cases Enabled

**Schema-Guided Value Setting:**
```
User: "Set temperature to 72 degrees"

LLM calls query_schema → Sees schema expects Celsius float + sensor_id

LLM: "The temperature record expects:
      - celsius (number, in Celsius)
      - sensor_id (string)
      - timestamp (number, unix timestamp)
      
      72°F is approximately 22.2°C. Which sensor should I update?
      (Current example uses 'sensor-01')"
```

**Validation Before Setting:**
```
User: "Set config to {port: 'abc'}"

LLM calls query_schema → Sees port should be integer

LLM: "Error: 'port' should be a number, not a string. 
      The schema expects: {port: integer, host: string}"
```

**Field Discovery:**
```
User: "What fields does the SensorData record have?"

LLM calls query_schema → Returns complete schema

LLM: "SensorData has these fields:
      - temperature (number) - required
      - humidity (number) - required  
      - timestamp (number, unix timestamp) - required
      - sensor_id (string) - required"
```

---

## Architecture Overview

### Component Diagram

```
┌────────────────────────────────────────────────────────┐
│              AimDB MCP Server                          │
│                                                        │
│  ┌─────────────────┐      ┌──────────────────┐       │
│  │  query_schema   │─────►│ Schema Analyzer  │       │
│  │     Tool        │      │ (JSON Inference) │       │
│  └─────────────────┘      └──────────┬───────┘       │
│                                      │                │
│                                      ▼                │
│                          ┌────────────────────┐       │
│                          │   AimX Client      │───────┼─► UDS
│                          │  - record.get      │       │
│                          │  - record.list     │       │
│                          └────────────────────┘       │
└────────────────────────────────────────────────────────┘
                                │
                                ▼
                   ┌─────────────────────────┐
                   │   AimDB Instance        │
                   │  - RecordMetadata       │
                   │  - Record Values        │
                   └─────────────────────────┘
```

### Data Flow

1. LLM calls `query_schema(record_name, socket_path)`
2. MCP server calls `record.list` to get metadata
3. MCP server calls `record.get` to fetch current value
4. Analyze JSON structure → Generate JSON Schema (single pass)
5. Merge with `RecordMetadata` (buffer info, counts)
6. Return comprehensive schema with metadata and example

---

## Schema Inference Algorithm

### Strategy: Value-Based Inference

Since AimDB doesn't store type information at the protocol level, we infer schema from:
1. **Current record value** (via `record.get`)
2. **RecordMetadata** (buffer type, capacity, producer/consumer counts)

### Basic Type Inference

```rust
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
                    "items": {}
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
```

**Design Notes:**
- **Performance**: Single pass, no recursion depth tracking needed
- **Assumptions**: Arrays are homogeneous (Rust's type system encourages this)
- **Simplicity**: No pattern matching - LLMs have field names and can ask users
- **Integer Detection**: Rely on serde_json's native type distinction

---

## Implementation Details

### Tool Parameters

```rust
// tools/aimdb-mcp/src/tools/schema.rs

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::error::{McpError, McpResult};

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
```

### Main Implementation

```rust
/// Query schema for a record
pub async fn query_schema(args: Option<Value>) -> McpResult<Value> {
    let params: QuerySchemaParams = parse_params(args)?;
    
    // Get connection to AimDB instance
    let pool = connection_pool().ok_or(McpError::NotInitialized)?;
    let mut client = pool.get(&params.socket_path).await?;
    
    // Fetch record metadata
    let metadata_list = client.list_records().await?;
    let metadata = metadata_list
        .iter()
        .find(|m| m.name == params.record_name)
        .ok_or_else(|| McpError::RecordNotFound(params.record_name.clone()))?;
    
    // Fetch current value (for schema inference)
    let value = client.get_record(&params.record_name).await?;
    
    // Infer JSON schema from value
    let schema = infer_json_schema(&value)?;
    
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
```

### Output Format

```json
{
  "record_name": "server::Temperature",
  "schema": {
    "type": "object",
    "properties": {
      "celsius": {
        "type": "number"
      },
      "sensor_id": {
        "type": "string"
      },
      "timestamp": {
        "type": "number"
      }
    },
    "required": ["celsius", "sensor_id", "timestamp"]
  },
  "metadata": {
    "type_id": "TypeId(0x1234567890abcdef)",
    "buffer_type": "spmc_ring",
    "buffer_capacity": 100,
    "writable": false,
    "producer_count": 1,
    "consumer_count": 3,
    "created_at": "2025-11-05T10:00:00.000000000Z",
    "last_update": "2025-11-05T19:45:30.123456789Z"
  },
  "example": {
    "celsius": 22.5,
    "sensor_id": "sensor-01",
    "timestamp": 1730649600.123
  },
  "inferred_at": "2025-11-05T19:45:00.000000000Z"
}
```

---

## Integration with MCP Server

### Tool Registration

```rust
// In tools/aimdb-mcp/src/server.rs

impl McpServer {
    pub fn list_tools(&self) -> Vec<Tool> {
        vec![
            // ... existing tools ...
            
            Tool {
                name: "query_schema".to_string(),
                description: "Get JSON schema and type information for a record.\n\n\
                    Returns the data structure, field types, and metadata.\n\
                    Use this before setting record values to understand expected format.\n\n\
                    Schema is inferred from current value + database metadata.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path (e.g., /tmp/aimdb.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Record name (e.g., server::Temperature)"
                        },
                        "include_example": {
                            "type": "boolean",
                            "description": "Include current value as example (default: true)",
                            "default": true
                        }
                    },
                    "required": ["socket_path", "record_name"],
                    "additionalProperties": false
                }),
            },
        ]
    }
    
    pub async fn handle_tool_call(&self, params: ToolCallParams) -> McpResult<Value> {
        match params.name.as_str() {
            // ... existing tool handlers ...
            "query_schema" => tools::query_schema(params.arguments).await?,
            _ => return Err(McpError::UnknownTool(params.name)),
        }
    }
}
```

### Module Structure

```rust
// tools/aimdb-mcp/src/tools/mod.rs

pub mod instance;
pub mod record;
pub mod subscription;
pub mod schema;  // NEW

// Re-export
pub use schema::query_schema;
```

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_primitive_types() {
        assert_eq!(
            infer_json_schema(&json!(42)).unwrap(),
            json!({"type": "integer"})
        );
        assert_eq!(
            infer_json_schema(&json!(3.14)).unwrap(),
            json!({"type": "number"})
        );
        assert_eq!(
            infer_json_schema(&json!("hello")).unwrap(),
            json!({"type": "string"})
        );
        assert_eq!(
            infer_json_schema(&json!(true)).unwrap(),
            json!({"type": "boolean"})
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
        assert_eq!(schema["required"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn test_infer_array_schema() {
        let value = json!([1, 2, 3]);
        let schema = infer_json_schema(&value).unwrap();
        
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "integer");
    }

    #[test]
    fn test_infer_nested_schema() {
        let value = json!({
            "sensor": {
                "id": "sensor-01",
                "location": "Room A"
            },
            "reading": 42.5
        });
        
        let schema = infer_json_schema(&value).unwrap();
        
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["sensor"]["type"], "object");
        assert_eq!(schema["properties"]["reading"]["type"], "number");
    }
}
```

### Integration Tests

```rust
// tests/schema_tests.rs

#[tokio::test]
async fn test_query_schema_integration() {
    // Start test AimDB instance
    let instance = TestInstance::start().await;
    
    // Query schema via MCP server
    let result = query_schema(Some(json!({
        "socket_path": instance.socket_path(),
        "record_name": "server::Temperature",
        "include_example": true
    }))).await.unwrap();
    
    // Verify schema structure
    assert_eq!(result["record_name"], "server::Temperature");
    assert!(result["schema"]["properties"].is_object());
    assert!(result["metadata"]["buffer_type"].is_string());
    assert!(result["example"].is_object());
    assert!(result["inferred_at"].is_string());
}

#[tokio::test]
async fn test_query_schema_without_example() {
    let instance = TestInstance::start().await;
    
    let result = query_schema(Some(json!({
        "socket_path": instance.socket_path(),
        "record_name": "server::Temperature",
        "include_example": false
    }))).await.unwrap();
    
    // Example should not be included
    assert!(result["example"].is_null());
}

#[tokio::test]
async fn test_query_schema_record_not_found() {
    let instance = TestInstance::start().await;
    
    let result = query_schema(Some(json!({
        "socket_path": instance.socket_path(),
        "record_name": "NonExistent::Record"
    }))).await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        McpError::RecordNotFound(name) => {
            assert_eq!(name, "NonExistent::Record");
        }
        _ => panic!("Expected RecordNotFound error"),
    }
}
```

---

## LLM Guidance and Instructions

### Recommended MCP Prompt Addition

Add this guidance to the MCP server prompts to help LLMs work with schemas:

```markdown
## Working with AimDB Schemas

When using `query_schema` to understand record structure:

1. **Field Names are Semantic**: Field names like `celsius`, `timestamp`, `sensor_id` 
   carry meaning. Use them to infer units and formats.

2. **Ask Users for Clarification**: If units or formats are unclear:
   - "I see a `temperature` field. Is this in Celsius or Fahrenheit?"
   - "The `timestamp` field is a number. Is this seconds or milliseconds since epoch?"
   - "Should I use the current time for the `timestamp` field?"

3. **Check Source Code**: For production systems, suggest checking the Rust type 
   definition for authoritative information about units and constraints.

4. **Use Example Values**: The `example` field shows a real value - use it as a 
   template for understanding the expected format.

5. **Respect Required Fields**: All fields in the `required` array must be provided 
   when setting a record value.

### Example LLM Behavior

**User:** "Set temperature to 75"

**Good LLM Response:**
```
I can see the record has a `celsius` field (type: number). Since you said "75", 
I want to confirm:
- Did you mean 75°F (which is 23.9°C)?
- Or did you mean 75°C directly?

The current value shows: {"celsius": 22.5, ...}
```

**User:** "Update the sensor reading"

**Good LLM Response:**
```
The SensorReading record has these fields:
- temperature (number)
- humidity (number) 
- timestamp (number)
- sensor_id (string)

What values should I set? I'll need all four fields since they're all required.
For timestamp, should I use the current time?
```
```

This guidance replaces pattern matching by teaching LLMs to be explicit rather than assume.

---

## Edge Cases and Error Handling

Given AimDB's architecture, most traditional edge cases don't apply:

### 1. Record Not Found

**Problem:** User queries schema for non-existent record.

**Solution:**
```rust
let metadata = metadata_list
    .iter()
    .find(|m| m.name == params.record_name)
    .ok_or_else(|| McpError::RecordNotFound(params.record_name.clone()))?;
```

**User Experience:**
```
Error: Record 'server::Config' not found

Available records:
- server::Temperature
- server::Humidity  
- server::Status
```

### 2. Connection Failures

**Problem:** Invalid socket path or connection timeout.

**Solution:**
```rust
let mut client = pool.get(&params.socket_path).await
    .map_err(|e| McpError::ConnectionFailed {
        socket_path: params.socket_path.clone(),
        reason: e.to_string(),
    })?;
```

**User Experience:**
```
Error: Cannot connect to /tmp/nonexistent.sock

Try: 
1. Use discover_instances to find available instances
2. Check if the AimDB server is running
```

### 3. Empty Arrays

**Problem:** Array with no elements - cannot infer item schema.

**Solution:**
```rust
Value::Array(arr) => {
    if arr.is_empty() {
        Ok(json!({
            "type": "array",
            "items": {},  // Unknown item type
            "description": "Empty array - item type unknown"
        }))
    } else {
        // Infer from first element
        let item_schema = infer_json_schema(&arr[0])?;
        Ok(json!({
            "type": "array",
            "items": item_schema
        }))
    }
}
```

### Non-Issues (Due to AimDB Architecture)

**❌ Empty Records**: Writable records require initialization, read-only records have producers

**❌ Schema Drift**: Rust types are static - schema cannot change at runtime

**❌ Deep Nesting**: Rust's type system makes excessively deep structures unlikely

**❌ Circular References**: Not possible in JSON representation

**❌ Type Ambiguity**: serde_json correctly distinguishes i64/u64/f64

---

## Future Enhancements

### 1. Multi-Sample Inference

**Current:** Infer from single value (current snapshot)

**Enhancement:**
- Sample multiple recent values via subscription
- Detect field consistency (always present vs. optional)
- Identify value ranges (min/max for numbers)
- Detect enum patterns (fixed set of string values)

**Use Case:** Detect optional fields in records with varying structure

### 2. Schema Validation Tool

**New tool: `validate_value`**
- Input: record name + proposed value
- Output: validation result + specific errors
- Use before `set_record` to avoid failures

**Example:**
```
LLM: validate_value({
  "record_name": "server::Temperature",
  "value": {"celsius": "not a number", "sensor_id": 123}
})

Result: {
  "valid": false,
  "errors": [
    {"field": "celsius", "error": "Expected number, got string"},
    {"field": "sensor_id", "error": "Expected string, got number"},
    {"field": "timestamp", "error": "Required field missing"}
  ]
}
```

### 3. Type Hints from Rust Doc Comments

**If using doc comments in Rust:**
```rust
/// Temperature reading in Celsius (valid range: -40 to 85)
pub celsius: f64,
```

Could extract hints if type metadata is preserved in AimX protocol (future core work).

---

## Implementation Checklist

### Core Functionality
- [ ] Create `tools/aimdb-mcp/src/tools/schema.rs`
- [ ] Implement `QuerySchemaParams` struct
- [ ] Implement `infer_json_schema()` function (single-pass, no recursion tracking)
- [ ] Implement `query_schema()` main function
- [ ] Add tool registration in `server.rs`
- [ ] Add tool handler in `server.rs`
- [ ] Export from `tools/mod.rs`

### Edge Case Handling
- [ ] Handle record not found with available records list
- [ ] Handle connection failures with helpful messages
- [ ] Handle empty arrays (unknown item type)

### Testing
- [ ] Unit tests: primitive types (number, string, boolean, null)
- [ ] Unit tests: object schemas
- [ ] Unit tests: array schemas (with elements and empty)
- [ ] Unit tests: nested structures
- [ ] Integration test: live AimDB instance
- [ ] Integration test: missing records error handling
- [ ] Integration test: connection failures
- [ ] Integration test: schema without example

### Documentation
- [ ] Update MCP server README with usage examples
- [ ] Add LLM guidance prompt (ask users for units/formats)
- [ ] Document performance characteristics (O(n) where n = JSON size)
- [ ] Add examples for common error scenarios

---
- [ ] Add tool handler in `server.rs`
- [ ] Export from `tools/mod.rs`

### Edge Case Handling
- [ ] Handle empty records (no value written yet)
- [ ] Enhanced integer vs float detection
- [ ] Deep nesting protection (max depth 20)
- [ ] Thread field names for nested pattern detection
- [ ] Handle special number values (NaN, Infinity)
- [ ] Improved error messages for connection failures
- [ ] Improved error messages for missing records
- [ ] Consider heterogeneous array handling (optional)

### Testing
- [ ] Unit tests: primitive types
- [ ] Unit tests: object schemas
- [ ] Unit tests: array schemas
- [ ] Unit tests: nested structures
- [ ] Unit tests: pattern detection (timestamp, ID, temperature)
- [ ] Unit tests: edge cases (empty records, deep nesting)
- [ ] Unit tests: integer vs float distinction
- [ ] Integration test: live AimDB instance
- [ ] Integration test: missing records error handling
- [ ] Integration test: schema without example

### Documentation
- [ ] Update MCP server README with usage examples
- [ ] Add to MCP prompts (schema-help prompt)
- [ ] Document edge case behavior in doc comments
- [ ] Add examples for common error scenarios

---

## Example LLM Conversations

### Example 1: Schema-Guided Setting (With User Clarification)

```
User: "Set the temperature to 75 degrees"

LLM: [Calls query_schema for "server::Temperature"]

LLM receives:
{
  "schema": {
    "properties": {
      "celsius": {"type": "number"},
      "sensor_id": {"type": "string"},
      "timestamp": {"type": "number"}
    },
    "required": ["celsius", "sensor_id", "timestamp"]
  },
  "example": {
    "celsius": 22.5,
    "sensor_id": "sensor-01",
    "timestamp": 1730649600.123
  }
}

LLM: "I can see the temperature record has a 'celsius' field. 
      Since you said '75 degrees':
      
      - Did you mean 75°F (approximately 23.9°C)?
      - Or did you mean 75°C directly?
      
      I also need:
      - sensor_id (currently 'sensor-01' in the example)
      - timestamp (should I use the current time?)
      
      Please clarify and I'll set the values."
```

### Example 2: Field Discovery

```
User: "What fields does the Config record have?"

LLM: [Calls query_schema for "server::Config"]

LLM: "The Config record has these fields:
     
     - port (integer) - required
     - host (string) - required
     - debug_mode (boolean) - required
     - max_connections (integer) - required
     
     Current values:
     - port: 8080
     - host: "0.0.0.0"
     - debug_mode: false
     - max_connections: 100"
```

---

**End of Document**
