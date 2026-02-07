//! MCP protocol-specific types
//!
//! Model Context Protocol version 2025-06-18

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP protocol version (preferred)
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Supported MCP protocol versions
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-06-18"];

/// Initialize request parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Protocol version requested by client
    pub protocol_version: String,
    /// Client capabilities
    pub capabilities: ClientCapabilities,
    /// Client info
    pub client_info: ClientInfo,
}

/// Client capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Sampling capability (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
}

/// Client info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Client name
    pub name: String,
    /// Client version
    pub version: String,
}

/// Initialize response result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Protocol version supported by server
    pub protocol_version: String,
    /// Server capabilities
    pub capabilities: ServerCapabilities,
    /// Server info
    pub server_info: ServerInfo,
}

/// Server capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// Tools capability (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Resources capability (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Prompts capability (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

/// Tools capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the tool list can change
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Resources capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    /// Whether subscriptions are supported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
}

/// Prompts capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    /// Whether the prompt list can change
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Server info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Server name
    pub name: String,
    /// Server version
    pub version: String,
    /// Optional metadata (custom server-specific information)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// Input schema (JSON Schema)
    pub input_schema: Value,
}

/// Tools list result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// Available tools
    pub tools: Vec<Tool>,
}

/// Tool call parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallParams {
    /// Tool name
    pub name: String,
    /// Tool arguments (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// Tool call result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Tool execution result content
    pub content: Vec<ToolContent>,
    /// Whether the tool call failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Tool content item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolContent {
    /// Text content
    Text { text: String },
    /// Image content (not used in aimdb-mcp)
    Image { data: String, mime_type: String },
    /// Resource content (not used in aimdb-mcp)
    Resource { resource: Value },
}

/// Prompt definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// Prompt name (unique identifier)
    pub name: String,
    /// Prompt description (human-readable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional arguments schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// Prompt argument definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptArgument {
    /// Argument name
    pub name: String,
    /// Argument description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this argument is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Prompts list result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsListResult {
    /// Available prompts
    pub prompts: Vec<Prompt>,
}

/// Prompts get parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsGetParams {
    /// Prompt name to retrieve
    pub name: String,
    /// Optional arguments for the prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// Prompts get result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsGetResult {
    /// Prompt description (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Prompt messages
    pub messages: Vec<PromptMessage>,
}

/// Prompt message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessage {
    /// Message role (user, assistant, system)
    pub role: String,
    /// Message content
    pub content: PromptMessageContent,
}

/// Prompt message content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessageContent {
    /// Content type (always "text" for now)
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content
    pub text: String,
}

/// Resource definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    /// Resource URI
    pub uri: String,
    /// Resource name (human-readable)
    pub name: String,
    /// Resource description (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Resources list result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesListResult {
    /// Available resources
    pub resources: Vec<Resource>,
}

/// Resource read parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReadParams {
    /// Resource URI to read
    pub uri: String,
}

/// Resource read result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReadResult {
    /// Resource contents
    pub contents: Vec<ResourceContent>,
}

/// Resource content item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContent {
    /// Resource URI
    pub uri: String,
    /// MIME type (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Text content (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Binary content as base64 (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

// ============================================================================
// Notifications
// ============================================================================

/// Notification message (server -> client)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// JSON-RPC version
    pub jsonrpc: String,
    /// Notification method
    pub method: String,
    /// Notification parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Create a new notification
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }

    /// Create a notification for initialized event
    pub fn initialized() -> Self {
        Self::new("notifications/initialized", None)
    }
}

// ============================================================================
// Resource Read Support
// ============================================================================
