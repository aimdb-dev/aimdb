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

/// Tools exposed in public mode (read-only, safe for untrusted clients).
const PUBLIC_TOOLS: &[&str] = &["discover_instances", "list_records", "get_record"];

/// Tools that change a running instance. Advertised and callable only with
/// `--allow-writes`, so an agent connected for introspection cannot write to
/// a live system on its own initiative.
const WRITE_TOOLS: &[&str] = &[
    "set_record",
    "reset_stage_profiling",
    "reset_buffer_metrics",
];

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
    /// When true, only PUBLIC_TOOLS are advertised and callable.
    public_mode: bool,
    /// When true, WRITE_TOOLS are advertised and callable (ignored in public mode).
    allow_writes: bool,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ServerState::Uninitialized)),
            connection_pool: ConnectionPool::new(),
            public_mode: false,
            allow_writes: false,
        }
    }

    /// Enable public mode: only read-only tools are available.
    pub fn with_public_mode(mut self, enabled: bool) -> Self {
        self.public_mode = enabled;
        self
    }

    /// Returns true if the server is in public (restricted) mode.
    pub fn is_public(&self) -> bool {
        self.public_mode
    }

    /// Offer the tools that change a running instance (`WRITE_TOOLS`).
    /// Public mode still wins: it never offers them.
    pub fn with_allow_writes(mut self, enabled: bool) -> Self {
        self.allow_writes = enabled;
        self
    }

    /// Returns true if the tools that change a running instance are offered.
    pub fn allows_writes(&self) -> bool {
        self.allow_writes && !self.public_mode
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

        // Build server capabilities — in public mode, only tools are available
        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
            resources: if self.public_mode {
                None
            } else {
                Some(ResourcesCapability {
                    subscribe: Some(false),
                })
            },
            prompts: if self.public_mode {
                None
            } else {
                Some(PromptsCapability {
                    list_changed: Some(false),
                })
            },
        };

        // Build server info (version from Cargo.toml). Advertise the prompts that
        // `prompts/list` actually returns, derived from the same source rather than
        // a hand-maintained list — empty in public mode, where prompts are disabled.
        let prompts_available: Vec<String> = if self.public_mode {
            Vec::new()
        } else {
            prompts::list_prompts()
                .into_iter()
                .map(|p| p.name)
                .collect()
        };
        let server_info = ServerInfo {
            name: "aimdb-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            metadata: Some(json!({
                "prompts_available": prompts_available,
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

        let mut tools = vec![
            Tool {
                name: "discover_instances".to_string(),
                description: "Discover all running AimDB instances on the system. Scans /tmp/*.sock and /var/run/aimdb/*.sock for AimDB servers. Each result carries the instance name, if one was given.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "list_records".to_string(),
                description: "List all records from a specific AimDB instance. Returns metadata including buffer type, capacity, producer/consumer counts, \
                    and the declared data contract as schema_type and schema_version. Those two fields \
                    are absent when the record was configured without a schema. Use get_record to \
                    see an observed value.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_record".to_string(),
                description: "Get the current value of a specific record from an AimDB instance. \
                    Returns {\"value\": <the record's JSON value>, \"ts\": <write time>}, where ts is \
                    nanoseconds since the AimDB instance started (not wall-clock time; only comparable \
                    within one instance lifetime).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to retrieve (e.g., server::Temperature)"
                        }
                    },
                    "required": ["record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "set_record".to_string(),
                description: "Set the value of a writable record in an AimDB instance. Only works for records with write permissions.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to update (must be writable)"
                        },
                        "value": {
                            "description": "New value for the record (must match record's type schema)"
                        }
                    },
                    "required": ["record_name", "value"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_instance_info".to_string(),
                description: "Get detailed information about a specific AimDB instance. Returns server version, protocol, permissions and capabilities, and from protocol 3.1 servers the AimDB version, the instance name and UID, and the build info embedded in the binary (application, version, git revision, uncommitted changes, kind, config hash, profile, target, post-link region state).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "drain_record".to_string(),
                description: "Drain all pending values from a record since the last drain call. \
                    Returns values in chronological order, each as {\"value\", \"ts\"} with ts the write \
                    time in nanoseconds since the AimDB instance started (not wall-clock time). \
                    This is a destructive read — \
                    drained values won't be returned again. Use this for batch analysis \
                    of accumulated data (e.g., time-series analysis, trend detection). \
                    The first drain call creates a reader and returns empty (cold start). \
                    Subsequent calls return all values accumulated since the previous drain.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
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
                    "required": ["record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_nodes".to_string(),
                description: "Get all nodes in the dependency graph. Returns metadata for all records as graph nodes, including origin (source/link/transform/passive), schema (name and version), buffer configuration, tap count, and the address of each outbound link. Useful for understanding database topology and data flow.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_edges".to_string(),
                description: "Get all edges in the dependency graph. Returns directed edges representing data flow between records. Shows how data flows from sources through transforms to consumers.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "graph_topo_order".to_string(),
                description: "Get the topological ordering of records in the dependency graph. Returns record keys ordered so all dependencies appear before their dependents. Reflects the spawn/initialization order used by AimDB.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_buffer_metrics".to_string(),
                description: "Get live buffer metrics for records matching a key string from a running AimDB instance.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        },
                        "record_key": {
                            "type": "string",
                            "description": "Substring to match against record names (e.g., 'Temperature')"
                        }
                    },
                    "required": ["record_key"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_stage_profiling".to_string(),
                description: "Get automatic stage profiling (per-`.source()`/`.tap()`/`.link()` callback wall-clock timing) for records matching a key from a running AimDB instance, including the slowest stage ('bottleneck'). Requires the instance to be built with the `profiling` feature.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        },
                        "record_key": {
                            "type": "string",
                            "description": "Substring to match against record names/keys (e.g., 'Temperature')"
                        }
                    },
                    "required": ["record_key"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "reset_stage_profiling".to_string(),
                description: "Reset stage profiling counters for every record on a running AimDB instance (requires write permission and the `profiling` feature).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "reset_buffer_metrics".to_string(),
                description: "Reset buffer introspection counters (produced/consumed/dropped/occupancy) for every record on a running AimDB instance (requires write permission and the `metrics` feature).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "endpoint": {
                            "type": "string",
                            "description": "Endpoint URL (unix://PATH, serial://DEVICE?baud=N) or a bare path. Falls back to AIMDB_CONNECT env var if omitted."
                        }
                    },
                    "additionalProperties": false
                }),
            },
        ];

        // In public mode, only expose the allowlisted tools
        if self.public_mode {
            tools.retain(|t| PUBLIC_TOOLS.contains(&t.name.as_str()));
        }

        // Tools that change a running instance are opt-in
        if !self.allows_writes() {
            tools.retain(|t| !WRITE_TOOLS.contains(&t.name.as_str()));
        }

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

        // Reject non-public tools in public mode (defense in depth)
        if self.public_mode && !PUBLIC_TOOLS.contains(&params.name.as_str()) {
            return Err(McpError::MethodNotFound(format!(
                "Unknown tool: {}",
                params.name
            )));
        }

        // Reject tools that change a running instance unless opted in, naming
        // the flag so the user knows how to enable them.
        if !self.allows_writes() && WRITE_TOOLS.contains(&params.name.as_str()) {
            return Err(McpError::MethodNotFound(format!(
                "Tool '{}' changes the running instance and is not offered; \
                 start aimdb-mcp with --allow-writes to enable it",
                params.name
            )));
        }

        // In public mode, strip any client-supplied endpoint so
        // resolve_endpoint falls back to the server-pinned --connect flag
        // or the AIMDB_CONNECT env var (never a client-chosen path).
        // This prevents clients from probing arbitrary Unix sockets on the host.
        let arguments = if self.public_mode {
            params.arguments.map(|mut v| {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("endpoint");
                }
                v
            })
        } else {
            params.arguments
        };
        let params = ToolCallParams {
            name: params.name,
            arguments,
        };

        let result = match params.name.as_str() {
            "discover_instances" => tools::discover_instances(params.arguments).await?,
            "list_records" => tools::list_records(params.arguments).await?,
            "get_record" => tools::get_record(params.arguments).await?,
            "set_record" => tools::set_record(params.arguments).await?,
            "get_instance_info" => tools::get_instance_info(params.arguments).await?,
            "drain_record" => tools::drain_record(params.arguments).await?,
            "graph_nodes" => tools::graph_nodes(params.arguments).await?,
            "graph_edges" => tools::graph_edges(params.arguments).await?,
            "graph_topo_order" => tools::graph_topo_order(params.arguments).await?,
            "get_buffer_metrics" => tools::get_buffer_metrics(params.arguments).await?,
            "reset_buffer_metrics" => tools::reset_buffer_metrics(params.arguments).await?,
            "get_stage_profiling" => tools::get_stage_profiling(params.arguments).await?,
            "reset_stage_profiling" => tools::reset_stage_profiling(params.arguments).await?,
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
        if self.public_mode {
            return Err(McpError::MethodNotFound("resources/list".to_string()));
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
        if self.public_mode {
            return Err(McpError::MethodNotFound("resources/read".to_string()));
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
        if self.public_mode {
            return Err(McpError::MethodNotFound("prompts/list".to_string()));
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
        if self.public_mode {
            return Err(McpError::MethodNotFound("prompts/get".to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_tools_allowlist_is_valid() {
        // Ensure the allowlist only contains tool names that actually exist in the
        // dispatch table, catching typos when tools are renamed. This list is a
        // snapshot — keep it in sync with the match arms in handle_tools_call().
        let known_tools = [
            "discover_instances",
            "list_records",
            "get_record",
            "set_record",
            "get_instance_info",
            "drain_record",
            "graph_nodes",
            "graph_edges",
            "graph_topo_order",
            "get_buffer_metrics",
            "reset_buffer_metrics",
            "get_stage_profiling",
            "reset_stage_profiling",
        ];
        for tool in PUBLIC_TOOLS {
            assert!(
                known_tools.contains(tool),
                "PUBLIC_TOOLS contains unknown tool: {tool}"
            );
        }
        for tool in WRITE_TOOLS {
            assert!(
                known_tools.contains(tool),
                "WRITE_TOOLS contains unknown tool: {tool}"
            );
        }
    }

    /// `query_schema` was retired (#420): it is offered in no mode and a call
    /// to it is an unknown tool.
    #[tokio::test]
    async fn query_schema_is_retired() {
        for server in [
            McpServer::new(),
            McpServer::new().with_allow_writes(true),
            McpServer::new().with_public_mode(true),
        ] {
            let names = tool_names(&server).await;
            assert!(!names.iter().any(|n| n == "query_schema"), "{names:?}");

            let params = ToolCallParams {
                name: "query_schema".to_string(),
                arguments: Some(json!({ "record_name": "r" })),
            };
            let err = server.handle_tools_call(params).await.unwrap_err();
            let McpError::MethodNotFound(message) = err else {
                panic!("expected MethodNotFound, got: {err:?}");
            };
            assert!(message.contains("Unknown tool: query_schema"), "{message}");
        }
    }

    async fn tool_names(server: &McpServer) -> Vec<String> {
        server.set_state(ServerState::Ready).await;
        let result = server.handle_tools_list().await.unwrap();
        result.tools.into_iter().map(|t| t.name).collect()
    }

    #[tokio::test]
    async fn default_mode_offers_reads_but_no_writes() {
        let names = tool_names(&McpServer::new()).await;
        for tool in WRITE_TOOLS {
            assert!(
                !names.iter().any(|n| n == tool),
                "{tool} offered by default"
            );
        }
        for tool in [
            "get_record",
            "graph_nodes",
            "get_buffer_metrics",
            "drain_record",
        ] {
            assert!(names.iter().any(|n| n == tool), "{tool} missing by default");
        }
    }

    #[tokio::test]
    async fn allow_writes_offers_write_tools() {
        let names = tool_names(&McpServer::new().with_allow_writes(true)).await;
        for tool in WRITE_TOOLS {
            assert!(
                names.iter().any(|n| n == tool),
                "{tool} missing with --allow-writes"
            );
        }
    }

    #[tokio::test]
    async fn default_mode_rejects_write_tool_naming_the_flag() {
        let server = McpServer::new();
        server.set_state(ServerState::Ready).await;
        for tool in WRITE_TOOLS {
            let params = ToolCallParams {
                name: tool.to_string(),
                arguments: None,
            };
            let err = server.handle_tools_call(params).await.unwrap_err();
            let McpError::MethodNotFound(message) = err else {
                panic!("expected MethodNotFound for {tool}, got: {err:?}");
            };
            assert!(message.contains("--allow-writes"), "{message}");
        }
    }

    #[tokio::test]
    async fn allow_writes_passes_write_tool_to_its_handler() {
        // No endpoint configured: the handler itself refuses with
        // InvalidParams, which shows the gate let the call through.
        std::env::remove_var("AIMDB_CONNECT");
        let server = McpServer::new().with_allow_writes(true);
        server.set_state(ServerState::Ready).await;
        let params = ToolCallParams {
            name: "set_record".to_string(),
            arguments: Some(json!({ "record_name": "r", "value": 1 })),
        };
        let result = server.handle_tools_call(params).await;
        assert!(
            !matches!(result, Err(McpError::MethodNotFound(_))),
            "set_record blocked despite --allow-writes: {result:?}"
        );
    }

    #[tokio::test]
    async fn public_mode_wins_over_allow_writes() {
        let server = McpServer::new()
            .with_public_mode(true)
            .with_allow_writes(true);
        assert!(!server.allows_writes());
        assert_eq!(tool_names(&server).await, PUBLIC_TOOLS);
    }

    #[test]
    fn public_mode_defaults_to_off() {
        let server = McpServer::new();
        assert!(!server.is_public());
    }

    #[test]
    fn public_mode_can_be_enabled() {
        let server = McpServer::new().with_public_mode(true);
        assert!(server.is_public());
    }

    #[tokio::test]
    async fn public_mode_filters_tools_list() {
        let server = McpServer::new().with_public_mode(true);
        server.set_state(ServerState::Ready).await;
        let result = server.handle_tools_list().await.unwrap();
        let names: Vec<&str> = result.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, PUBLIC_TOOLS);
    }

    #[tokio::test]
    async fn public_mode_rejects_non_public_tool() {
        let server = McpServer::new().with_public_mode(true);
        server.set_state(ServerState::Ready).await;
        let params = ToolCallParams {
            name: "set_record".to_string(),
            arguments: None,
        };
        let err = server.handle_tools_call(params).await.unwrap_err();
        assert!(matches!(err, McpError::MethodNotFound(_)));
    }

    // Helper: assert that an explicit endpoint is stripped in public mode.
    // The stripping is confirmed by getting InvalidParams (no socket configured)
    // rather than a connection error to the attacker-supplied path.
    async fn assert_endpoint_stripped(tool: &str) {
        // Clear env so it doesn't interfere with the expected InvalidParams result.
        std::env::remove_var("AIMDB_CONNECT");

        let server = McpServer::new().with_public_mode(true);
        server.set_state(ServerState::Ready).await;
        let params = ToolCallParams {
            name: tool.to_string(),
            arguments: Some(json!({ "endpoint": "/tmp/evil.sock" })),
        };
        let err = server.handle_tools_call(params).await.unwrap_err();
        assert!(
            matches!(err, McpError::InvalidParams(_)),
            "expected InvalidParams for {tool}, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn public_mode_strips_endpoint_list_records() {
        assert_endpoint_stripped("list_records").await;
    }

    #[tokio::test]
    async fn public_mode_strips_endpoint_get_record() {
        assert_endpoint_stripped("get_record").await;
    }

    #[tokio::test]
    async fn public_mode_strips_endpoint_discover_instances() {
        // discover_instances scans the filesystem directly — it doesn't call
        // resolve_endpoint, so stripping endpoint doesn't cause InvalidParams.
        // The expected outcome is that the tool runs normally (no instances found in
        // the test environment), confirming the evil endpoint was not connected to.
        let server = McpServer::new().with_public_mode(true);
        server.set_state(ServerState::Ready).await;
        let params = ToolCallParams {
            name: "discover_instances".to_string(),
            arguments: Some(json!({ "endpoint": "/tmp/evil.sock" })),
        };
        let result = server.handle_tools_call(params).await;
        // The tool is allowed (not MethodNotFound) and does not attempt to connect
        // to the evil socket. Either Ok or a no-instances error are both acceptable.
        assert!(
            !matches!(result, Err(McpError::MethodNotFound(_))),
            "discover_instances should not be blocked in public mode"
        );
    }

    #[tokio::test]
    async fn normal_mode_lists_all_tools() {
        let server = McpServer::new();
        server.set_state(ServerState::Ready).await;
        let result = server.handle_tools_list().await.unwrap();
        assert!(result.tools.len() > PUBLIC_TOOLS.len());
    }

    #[tokio::test]
    async fn public_mode_suppresses_resources_and_prompts() {
        let server = McpServer::new().with_public_mode(true);
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: crate::protocol::ClientCapabilities { sampling: None },
            client_info: crate::protocol::ClientInfo {
                name: "test".to_string(),
                version: "0.1".to_string(),
            },
        };
        let result = server.handle_initialize(params).await.unwrap();
        assert!(result.capabilities.tools.is_some());
        assert!(result.capabilities.resources.is_none());
        assert!(result.capabilities.prompts.is_none());
    }
}
