//! Error types for the MCP server

use thiserror::Error;

/// Result type for MCP operations
pub type McpResult<T> = Result<T, McpError>;

/// Errors that can occur in the MCP server
#[derive(Debug, Error)]
pub enum McpError {
    /// IO error (stdin/stdout)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// AimDB client error
    #[error("AimDB client error: {0}")]
    Client(#[from] aimdb_client::ClientError),

    /// Invalid JSON-RPC request
    #[error("Invalid JSON-RPC request: {0}")]
    InvalidRequest(String),

    /// Method not found
    #[error("Method not found: {0}")]
    MethodNotFound(String),

    /// Invalid parameters
    #[error("Invalid parameters: {0}")]
    InvalidParams(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),

    /// Protocol not initialized
    #[error("Protocol not initialized - call initialize first")]
    NotInitialized,

    /// Protocol version mismatch
    #[error("Unsupported protocol version: {0}")]
    UnsupportedProtocol(String),
}

impl McpError {
    /// Convert error to JSON-RPC error code
    pub fn error_code(&self) -> i32 {
        match self {
            McpError::InvalidRequest(_) => -32600,
            McpError::MethodNotFound(_) => -32601,
            McpError::InvalidParams(_) => -32602,
            McpError::Internal(_) => -32603,
            McpError::NotInitialized => -32002,
            McpError::UnsupportedProtocol(_) => -32003,
            _ => -32000, // Server error
        }
    }

    /// Get error message for JSON-RPC response
    pub fn message(&self) -> String {
        self.to_string()
    }
}
