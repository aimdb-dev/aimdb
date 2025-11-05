//! JSON-RPC 2.0 types
//!
//! See: https://www.jsonrpc.org/specification

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request ID (string or number)
    pub id: Option<Value>,
    /// Method name
    pub method: String,
    /// Method parameters (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response (success)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request ID (must match request)
    pub id: Value,
    /// Result value
    pub result: Value,
}

/// JSON-RPC 2.0 error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request ID (must match request, or null if parse error)
    pub id: Value,
    /// Error object
    pub error: JsonRpcError,
}

/// JSON-RPC 2.0 error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code
    pub code: i32,
    /// Error message
    pub message: String,
    /// Additional error data (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 notification (no response expected)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Method name
    pub method: String,
    /// Method parameters (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Create a new request
    pub fn new(id: Value, method: String, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method,
            params,
        }
    }
}

impl JsonRpcResponse {
    /// Create a new success response
    pub fn new(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result,
        }
    }
}

impl JsonRpcErrorResponse {
    /// Create a new error response
    pub fn new(id: Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            error,
        }
    }
}

impl JsonRpcError {
    /// Create a new error
    pub fn new(code: i32, message: String, data: Option<Value>) -> Self {
        Self {
            code,
            message,
            data,
        }
    }
}

impl JsonRpcNotification {
    /// Create a new notification
    pub fn new(method: String, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method,
            params,
        }
    }
}
