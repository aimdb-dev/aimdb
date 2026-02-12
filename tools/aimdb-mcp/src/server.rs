//! MCP server implementation
//!
//! Handles MCP protocol lifecycle, tool dispatch, and resource management.

use crate::connection::ConnectionPool;
use crate::error::{McpError, McpResult};
use crate::protocol::{
    InitializeParams, InitializeResult, PromptsCapability, PromptsGetParams, PromptsGetResult,
    PromptsListResult, ResourceReadParams, ResourceReadResult, ResourcesCapability,
    ResourcesListResult, ServerCapabilities, ServerInfo, Tool, ToolCallParams, ToolCallResult,
    ToolContent, ToolsCapability, ToolsListResult, MCP_PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::{prompts, resources, tools};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// MCP server state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Server created but not initialized
    Uninitialized,
    /// Server initialized and ready to handle requests
    Ready,
    /// Server closed
    Closed,
}

/// MCP server
pub struct McpServer {
    state: Arc<Mutex<ServerState>>,
    connection_pool: ConnectionPool,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ServerState::Uninitialized)),
            connection_pool: ConnectionPool::new(),
        }
    }

    /// Get the connection pool
    pub fn connection_pool(&self) -> &ConnectionPool {
        &self.connection_pool
    }

    /// Get current server state
    pub async fn state(&self) -> ServerState {
        *self.state.lock().await
    }

    /// Check if server is ready
    pub async fn is_ready(&self) -> bool {
        self.state().await == ServerState::Ready
    }

    /// Set server state (internal use)
    #[allow(dead_code)]
    pub(crate) async fn set_state(&self, new_state: ServerState) {
        *self.state.lock().await = new_state;
    }

    /// Handle MCP initialize request
    ///
    /// This is the first method that must be called by the client.
    /// It negotiates protocol version and capabilities.
    pub async fn handle_initialize(&self, params: InitializeParams) -> McpResult<InitializeResult> {
        // Verify protocol version - support multiple versions for compatibility
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&params.protocol_version.as_str()) {
            return Err(McpError::UnsupportedProtocol(params.protocol_version));
        }

        // Set server to ready state
        self.set_state(ServerState::Ready).await;

        // Initialize connection pool for tools (if not already done)
        tools::init_connection_pool(self.connection_pool.clone());

        // Build server capabilities
        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false), // Tool list is static for now
            }),
            resources: Some(ResourcesCapability {
                subscribe: Some(false),
            }),
            prompts: Some(PromptsCapability {
                list_changed: Some(false), // Prompt list is static
            }),
        };

        // Build server info (version from Cargo.toml)
        let server_info = ServerInfo {
            name: "aimdb-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            metadata: Some(json!({
                "prompts_available": ["schema-help", "troubleshooting"],
            })),
        };

        Ok(InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities,
            server_info,
        })
    }

    /// Handle tools/list request
    ///
    /// Returns the list of available tools with their schemas.
    pub async fn handle_tools_list(&self) -> McpResult<ToolsListResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("📋 Listing available tools");

        let tools = vec![
            Tool {
                name: "discover_instances".to_string(),
                description: "Discover all running AimDB instances on the system. Scans /tmp/*.sock and /var/run/aimdb/*.sock for AimDB servers.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "list_records".to_string(),
                description: "List all records from a specific AimDB instance. Returns metadata including buffer type, capacity, producer/consumer counts, and timestamps.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_record".to_string(),
                description: "Get the current value of a specific record from an AimDB instance. Returns the record's current JSON value.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to retrieve (e.g., server::Temperature)"
                        }
                    },
                    "required": ["socket_path", "record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "set_record".to_string(),
                description: "Set the value of a writable record in an AimDB instance. Only works for records with write permissions.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to update (must be writable)"
                        },
                        "value": {
                            "description": "New value for the record (must match record's type schema)"
                        }
                    },
                    "required": ["socket_path", "record_name", "value"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_instance_info".to_string(),
                description: "Get detailed information about a specific AimDB instance. Returns server version, protocol, permissions, and capabilities.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "query_schema".to_string(),
                description: "Get JSON schema and type information for a record.\n\n\
                    Returns the data structure, field types, and metadata.\n\
                    Use this before setting record values to understand expected format.\n\n\
                    Schema is inferred from current value + database metadata.\n\n\
                    💡 TIP: Field names like 'celsius', 'timestamp', 'sensor_id' carry semantic meaning.\n\
                    If units or formats are unclear, ask the user for clarification.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to query schema for (e.g., server::Temperature)"
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
            Tool {
                name: "drain_record".to_string(),
                description: "Drain all pending values from a record since the last drain call. \
                    Returns values in chronological order. This is a destructive read — \
                    drained values won't be returned again. Use this for batch analysis \
                    of accumulated data (e.g., time-series analysis, trend detection). \
                    The first drain call creates a reader and returns empty (cold start). \
                    Subsequent calls return all values accumulated since the previous drain.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to drain (e.g., temp.berlin)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of values to drain. Optional, defaults to all pending.",
                            "minimum": 1
                        }
                    },
                    "required": ["socket_path", "record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_nodes".to_string(),
                description: "Get all nodes in the dependency graph. Returns metadata for all records as graph nodes, including origin (source/link/transform/passive), buffer configuration, and connection counts. Useful for understanding database topology and data flow.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_edges".to_string(),
                description: "Get all edges in the dependency graph. Returns directed edges representing data flow between records. Shows how data flows from sources through transforms to consumers.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_topo_order".to_string(),
                description: "Get the topological ordering of records in the dependency graph. Returns record keys ordered so all dependencies appear before their dependents. Reflects the spawn/initialization order used by AimDB.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
        ];

        Ok(ToolsListResult { tools })
    }

    /// Handle tools/call request
    ///
    /// Dispatches tool calls to the appropriate handler.
    pub async fn handle_tools_call(&self, params: ToolCallParams) -> McpResult<ToolCallResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("🛠️  Calling tool: {}", params.name);

        let result = match params.name.as_str() {
            "discover_instances" => tools::discover_instances(params.arguments).await?,
            "list_records" => tools::list_records(params.arguments).await?,
            "get_record" => tools::get_record(params.arguments).await?,
            "set_record" => tools::set_record(params.arguments).await?,
            "get_instance_info" => tools::get_instance_info(params.arguments).await?,
            "query_schema" => tools::query_schema(params.arguments).await?,
            "drain_record" => tools::drain_record(params.arguments).await?,
            "graph_nodes" => tools::graph_nodes(params.arguments).await?,
            "graph_edges" => tools::graph_edges(params.arguments).await?,
            "graph_topo_order" => tools::graph_topo_order(params.arguments).await?,
            _ => {
                return Err(McpError::MethodNotFound(format!(
                    "Unknown tool: {}",
                    params.name
                )));
            }
        };

        // Wrap result in ToolCallResult
        let content = vec![ToolContent::Text {
            text: serde_json::to_string_pretty(&result)?,
        }];

        Ok(ToolCallResult {
            content,
            is_error: Some(false),
        })
    }

    /// Handle resources/list request
    ///
    /// Returns the list of available resources.
    pub async fn handle_resources_list(&self) -> McpResult<ResourcesListResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("📋 Handling resources/list");
        resources::list_resources().await
    }

    /// Handle resources/read request
    ///
    /// Reads the content of a specific resource by URI.
    pub async fn handle_resources_read(
        &self,
        params: ResourceReadParams,
    ) -> McpResult<ResourceReadResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("📖 Handling resources/read: {}", params.uri);
        resources::read_resource(&params.uri).await
    }

    /// Handle prompts/list request
    ///
    /// Returns the list of available prompts.
    pub async fn handle_prompts_list(&self) -> McpResult<PromptsListResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("📋 Listing available prompts");

        let prompts = prompts::list_prompts();

        Ok(PromptsListResult { prompts })
    }

    /// Handle prompts/get request
    ///
    /// Returns a specific prompt with its messages.
    pub async fn handle_prompts_get(
        &self,
        params: PromptsGetParams,
    ) -> McpResult<PromptsGetResult> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("📝 Getting prompt: {}", params.name);

        let messages = prompts::get_prompt(&params.name)
            .ok_or_else(|| McpError::InvalidParams(format!("Unknown prompt: {}", params.name)))?;

        Ok(PromptsGetResult {
            description: Some(format!("Prompt: {}", params.name)),
            messages,
        })
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}
