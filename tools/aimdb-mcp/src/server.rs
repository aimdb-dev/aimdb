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

        // Initialize session store for architecture agent
        crate::architecture::init_session_store();

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
            // ── Architecture agent tools (M11) ─────────────────────────────
            Tool {
                name: "get_architecture".to_string(),
                description: "Return the current architecture state from .aimdb/state.toml as structured JSON, including record count, validation summary, and decision log length. Run this first when entering an architecture session.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "state_path": {
                            "type": "string",
                            "description": "Path to state.toml (default: .aimdb/state.toml)"
                        }
                    },
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "propose_add_record".to_string(),
                description: "Propose adding a new record to the architecture. All payload fields are explicit and typed — no guessing required. Present the proposal to the user before calling resolve_proposal.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "PascalCase record name, e.g. \"TemperatureReading\""
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the proposal shown to the user"
                        },
                        "buffer": {
                            "type": "string",
                            "enum": ["SpmcRing", "SingleLatest", "Mailbox"],
                            "description": "Buffer semantics: SpmcRing=stream (every value), SingleLatest=state (newest only), Mailbox=command (overwrite)"
                        },
                        "capacity": {
                            "type": "integer",
                            "description": "Ring buffer capacity — required when buffer=SpmcRing. Use power-of-2, e.g. 256, 512, 1024."
                        },
                        "key_prefix": {
                            "type": "string",
                            "description": "Optional common key prefix, e.g. \"sensors.temp.\". Default: \"\""
                        },
                        "key_variants": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Concrete PascalCase variant names, e.g. [\"Default\"] or [\"Indoor\", \"Outdoor\"]. Default: []"
                        },
                        "producers": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Task names that write to this record, e.g. [\"sensor_task\"]."
                        },
                        "consumers": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Task names that read from this record, e.g. [\"anomaly_detector\"]."
                        },
                        "fields": {
                            "type": "array",
                            "description": "Value struct fields",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "snake_case field name" },
                                    "type": { "type": "string", "description": "Rust primitive: f64, f32, u8, u16, u32, u64, i8, i16, i32, i64, bool, String" },
                                    "description": { "type": "string" }
                                },
                                "required": ["name", "type", "description"]
                            }
                        },
                        "connectors": {
                            "type": "array",
                            "description": "Connector wiring (MQTT, KNX, etc.)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "protocol": { "type": "string", "description": "e.g. mqtt, knx" },
                                    "direction": { "type": "string", "enum": ["inbound", "outbound"] },
                                    "url": { "type": "string", "description": "Topic/address template; may contain {variant}" }
                                },
                                "required": ["protocol", "direction", "url"]
                            }
                        }
                    },
                    "required": ["name", "description", "buffer"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "propose_modify_buffer".to_string(),
                description: "Propose changing the buffer type (and optionally capacity) of an existing record. Present the proposal to the user before calling resolve_proposal.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "record_name": {
                            "type": "string",
                            "description": "PascalCase name of the existing record to modify"
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the proposal shown to the user"
                        },
                        "buffer": {
                            "type": "string",
                            "enum": ["SpmcRing", "SingleLatest", "Mailbox"],
                            "description": "New buffer type"
                        },
                        "capacity": {
                            "type": "integer",
                            "description": "Ring capacity — required when buffer=SpmcRing"
                        }
                    },
                    "required": ["record_name", "description", "buffer"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "propose_add_connector".to_string(),
                description: "Propose adding a connector (MQTT, KNX, etc.) to an existing record. Present the proposal to the user before calling resolve_proposal.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "record_name": {
                            "type": "string",
                            "description": "PascalCase name of the existing record to wire up"
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the proposal shown to the user"
                        },
                        "protocol": {
                            "type": "string",
                            "description": "Connector protocol identifier, e.g. \"mqtt\" or \"knx\""
                        },
                        "direction": {
                            "type": "string",
                            "enum": ["inbound", "outbound"],
                            "description": "inbound = broker→DB, outbound = DB→broker"
                        },
                        "url": {
                            "type": "string",
                            "description": "Topic or address template; use {variant} placeholder for key variants, e.g. \"sensors/temp/{variant}\""
                        }
                    },
                    "required": ["record_name", "description", "protocol", "direction", "url"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "propose_modify_fields".to_string(),
                description: "Propose replacing the value struct fields of an existing record. This replaces ALL fields — include unchanged fields too. Present the proposal to the user before calling resolve_proposal.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "record_name": {
                            "type": "string",
                            "description": "PascalCase name of the existing record to modify"
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the proposal shown to the user"
                        },
                        "fields": {
                            "type": "array",
                            "description": "Complete replacement field list for the value struct",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "snake_case field name" },
                                    "type": { "type": "string", "description": "f64, f32, u8, u16, u32, u64, i8, i16, i32, i64, bool, String" },
                                    "description": { "type": "string" }
                                },
                                "required": ["name", "type", "description"]
                            }
                        }
                    },
                    "required": ["record_name", "description", "fields"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "propose_modify_key_variants".to_string(),
                description: "Propose updating the key variants of an existing record. Use this when adding a record with no variants (e.g. [\"Default\"]) or expanding a fleet (e.g. adding a new device). Present the proposal to the user before calling resolve_proposal.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "record_name": {
                            "type": "string",
                            "description": "PascalCase name of the existing record to modify"
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the proposal shown to the user"
                        },
                        "key_variants": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Complete replacement list of PascalCase variant names, e.g. [\"Default\"] or [\"ApiServer\", \"Worker\", \"Db\"]. Replaces prior variant list."
                        },
                        "key_prefix": {
                            "type": "string",
                            "description": "Optional common key prefix. If omitted the existing prefix is preserved."
                        }
                    },
                    "required": ["record_name", "description", "key_variants"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "resolve_proposal".to_string(),
                description: "Resolve a pending proposal. On confirm: applies the change, writes state.toml, generates Mermaid and Rust artefacts. On reject: discards without changes. On revise: discards with a redirect message.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "proposal_id": {
                            "type": "string",
                            "description": "The proposal ID returned by propose_add_record, propose_modify_buffer, propose_add_connector, propose_modify_fields, propose_modify_key_variants, remove_record, or rename_record"
                        },
                        "resolution": {
                            "type": "string",
                            "enum": ["confirm", "reject", "revise"],
                            "description": "User decision: confirm applies the change, reject discards it, revise returns a redirect"
                        },
                        "redirect": {
                            "type": "string",
                            "description": "Message explaining what to revise (only used when resolution=revise)"
                        },
                        "state_path": { "type": "string", "description": "Override state.toml path" },
                        "mermaid_path": { "type": "string", "description": "Override Mermaid output path" },
                        "rust_path": { "type": "string", "description": "Override Rust output path" }
                    },
                    "required": ["proposal_id", "resolution"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "remove_record".to_string(),
                description: "Propose removal of an existing record. Creates a pending proposal — call resolve_proposal to confirm. Note: removing a record breaks generated type aliases.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "record_name": {
                            "type": "string",
                            "description": "PascalCase name of the record to remove"
                        }
                    },
                    "required": ["record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "rename_record".to_string(),
                description: "Propose renaming a record. Creates a pending proposal — call resolve_proposal to confirm. Note: renames the generated key enum and value struct, breaking existing references.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "old_name": {
                            "type": "string",
                            "description": "Current PascalCase record name"
                        },
                        "new_name": {
                            "type": "string",
                            "description": "New PascalCase record name"
                        }
                    },
                    "required": ["old_name", "new_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "validate_against_instance".to_string(),
                description: "Compare state.toml against a live AimDB instance and return a conflict report. Detects missing records, buffer type mismatches, capacity differences, and connector mismatches.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the running AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "state_path": {
                            "type": "string",
                            "description": "Path to state.toml (default: .aimdb/state.toml)"
                        }
                    },
                    "required": ["socket_path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_buffer_metrics".to_string(),
                description: "Get live buffer metrics for records matching a key string from a running AimDB instance.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance"
                        },
                        "record_key": {
                            "type": "string",
                            "description": "Substring to match against record names (e.g., 'Temperature')"
                        }
                    },
                    "required": ["socket_path", "record_key"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "save_memory".to_string(),
                description: "Persist ideation context and design rationale to .aimdb/memory.md. \
                    Call this after every confirmed proposal with a narrative summary of what the user is building, \
                    the key question asked, the answer received, why the chosen buffer type fits, \
                    alternatives that were considered and rejected, and any future considerations noted. \
                    On session start, read aimdb://architecture/memory to restore this context.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "entry": {
                            "type": "string",
                            "description": "Markdown text to write. For append mode, structure as a '## RecordName' section with sub-headings: Context, Key question, Answer, Buffer choice & rationale, Alternatives considered, Future considerations."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["append", "overwrite"],
                            "description": "append (default): add a timestamped section to memory.md. overwrite: replace the entire file (use only to correct the whole document)."
                        },
                        "memory_path": {
                            "type": "string",
                            "description": "Override path (default: .aimdb/memory.md)"
                        }
                    },
                    "required": ["entry"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "reset_session".to_string(),
                description: "Reset the architecture agent session, discarding any pending proposals. Use when the user wants to start over or abandon the current ideation cycle.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
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
            // Architecture agent tools (M11)
            "get_architecture" => tools::get_architecture(params.arguments).await?,
            "propose_add_record" => tools::propose_add_record(params.arguments).await?,
            "propose_modify_buffer" => tools::propose_modify_buffer(params.arguments).await?,
            "propose_add_connector" => tools::propose_add_connector(params.arguments).await?,
            "propose_modify_fields" => tools::propose_modify_fields(params.arguments).await?,
            "propose_modify_key_variants" => {
                tools::propose_modify_key_variants(params.arguments).await?
            }
            "resolve_proposal" => tools::resolve_proposal(params.arguments).await?,
            "remove_record" => tools::remove_record(params.arguments).await?,
            "rename_record" => tools::rename_record(params.arguments).await?,
            "validate_against_instance" => {
                tools::validate_against_instance(params.arguments).await?
            }
            "get_buffer_metrics" => tools::get_buffer_metrics(params.arguments).await?,
            "save_memory" => tools::save_memory(params.arguments).await?,
            "reset_session" => tools::reset_session(params.arguments).await?,
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
