//! MCP server implementation
//!
//! Handles MCP protocol lifecycle, tool dispatch, and resource management.

use crate::connection::ConnectionPool;
use crate::error::{McpError, McpResult};
use crate::protocol::{
    InitializeParams, InitializeResult, Notification, ResourceReadParams, ResourceReadResult,
    ResourcesCapability, ResourcesListResult, ServerCapabilities, ServerInfo, Tool, ToolCallParams,
    ToolCallResult, ToolContent, ToolsCapability, ToolsListResult, MCP_PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::subscription_manager::SubscriptionManager;
use crate::{resources, tools};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

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
    subscription_manager: Arc<SubscriptionManager>,
    #[allow(dead_code)] // TODO: Phase 3 - Wire up notification forwarding to stdio
    notification_tx: mpsc::UnboundedSender<Notification>,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Notification>) {
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let subscription_manager = Arc::new(SubscriptionManager::new(notification_tx.clone()));

        let server = Self {
            state: Arc::new(Mutex::new(ServerState::Uninitialized)),
            connection_pool: ConnectionPool::new(),
            subscription_manager,
            notification_tx,
        };

        (server, notification_rx)
    }

    /// Get the connection pool
    pub fn connection_pool(&self) -> &ConnectionPool {
        &self.connection_pool
    }

    /// Get the subscription manager
    pub fn subscription_manager(&self) -> &Arc<SubscriptionManager> {
        &self.subscription_manager
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

        // Initialize subscription manager for tools (if not already done)
        tools::init_subscription_manager(self.subscription_manager.clone());

        // Build server capabilities
        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false), // Tool list is static for now
            }),
            resources: Some(ResourcesCapability {
                subscribe: Some(true), // Phase 3: Resource subscriptions via subscribe_record tool
            }),
            prompts: None, // Prompts not yet implemented
        };

        // Build server info (version from Cargo.toml)
        let server_info = ServerInfo {
            name: "aimdb-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
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
                name: "subscribe_record".to_string(),
                description: "Subscribe to real-time updates for a specific record. Sends notifications when the record value changes.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "socket_path": {
                            "type": "string",
                            "description": "Unix socket path to the AimDB instance (e.g., /tmp/aimdb-demo.sock)"
                        },
                        "record_name": {
                            "type": "string",
                            "description": "Name of the record to subscribe to (e.g., server::Temperature)"
                        }
                    },
                    "required": ["socket_path", "record_name"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "unsubscribe_record".to_string(),
                description: "Unsubscribe from record updates and stop receiving notifications.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "subscription_id": {
                            "type": "string",
                            "description": "Subscription ID to cancel (returned from subscribe_record)"
                        }
                    },
                    "required": ["subscription_id"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "list_subscriptions".to_string(),
                description: "List all active subscriptions with their metadata.".to_string(),
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
            "subscribe_record" => tools::subscribe_record(params.arguments).await?,
            "unsubscribe_record" => tools::unsubscribe_record(params.arguments).await?,
            "list_subscriptions" => tools::list_subscriptions(params.arguments).await?,
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

    /// Handle resources/subscribe request
    ///
    /// Subscribes to a resource URI. For record URIs, this creates a subscription
    /// to the underlying record and returns notifications when it changes.
    ///
    /// Supported URIs:
    /// - `aimdb://<socket>/records/<record_name>` - Subscribe to specific record
    pub async fn handle_resources_subscribe(&self, uri: String) -> McpResult<String> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("🔔 Handling resources/subscribe: {}", uri);

        // Parse URI to extract socket path and record name
        // Format: aimdb://<socket>/records/<record_name>
        if !uri.starts_with("aimdb://") {
            return Err(McpError::InvalidParams(format!(
                "Invalid resource URI: {}. Must start with aimdb://",
                uri
            )));
        }

        let path_part = &uri[8..]; // Skip "aimdb://"
        let parts: Vec<&str> = path_part.split("/records/").collect();

        if parts.len() != 2 {
            return Err(McpError::InvalidParams(format!(
                "Invalid resource URI: {}. Expected format: aimdb://<socket>/records/<record_name>",
                uri
            )));
        }

        let socket_path = parts[0];
        let record_name = parts[1];

        debug!(
            "Subscribing to record {} at socket {}",
            record_name, socket_path
        );

        // Use subscription manager to create subscription
        let manager = self.subscription_manager();
        let subscription_id = manager
            .subscribe(
                std::path::PathBuf::from(socket_path),
                record_name.to_string(),
                10, // queue_size
            )
            .await?;

        info!(
            "✅ Resource subscription created: {} -> {}",
            uri, subscription_id
        );

        Ok(subscription_id)
    }

    /// Handle resources/unsubscribe request
    ///
    /// Unsubscribes from a resource by subscription ID.
    pub async fn handle_resources_unsubscribe(&self, subscription_id: String) -> McpResult<()> {
        if !self.is_ready().await {
            return Err(McpError::NotInitialized);
        }

        debug!("🔕 Handling resources/unsubscribe: {}", subscription_id);

        let manager = self.subscription_manager();
        manager.unsubscribe(&subscription_id).await?;

        info!("✅ Resource unsubscribed: {}", subscription_id);

        Ok(())
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new().0
    }
}
