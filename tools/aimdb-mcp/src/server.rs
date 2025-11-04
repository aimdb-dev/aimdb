//! MCP server implementation
//!
//! Handles MCP protocol lifecycle, tool dispatch, and resource management.

use crate::connection::ConnectionPool;
use crate::error::{McpError, McpResult};
use crate::protocol::{
    InitializeParams, InitializeResult, Notification, PromptsCapability, PromptsGetParams,
    PromptsGetResult, PromptsListResult, ResourceReadParams, ResourceReadResult,
    ResourcesCapability, ResourcesListResult, ServerCapabilities, ServerInfo, Tool, ToolCallParams,
    ToolCallResult, ToolContent, ToolsCapability, ToolsListResult, MCP_PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::subscription_manager::SubscriptionManager;
use crate::{prompts, resources, tools};
use serde_json::json;
use std::path::PathBuf;
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
    notification_dir: Arc<PathBuf>,
    #[allow(dead_code)] // TODO: Phase 3 - Wire up notification forwarding to stdio
    notification_tx: mpsc::UnboundedSender<Notification>,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new(notification_dir: PathBuf) -> (Self, mpsc::UnboundedReceiver<Notification>) {
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let subscription_manager = Arc::new(SubscriptionManager::new(notification_tx.clone()));

        let server = Self {
            state: Arc::new(Mutex::new(ServerState::Uninitialized)),
            connection_pool: ConnectionPool::new(),
            subscription_manager,
            notification_dir: Arc::new(notification_dir),
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

        // Initialize notification directory for tools (if not already done)
        tools::init_notification_dir((*self.notification_dir).clone());

        // Build server capabilities
        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false), // Tool list is static for now
            }),
            resources: Some(ResourcesCapability {
                subscribe: Some(true), // Phase 3: Resource subscriptions via subscribe_record tool
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
                "notification_directory": self.notification_dir.display().to_string(),
                "prompts_available": ["notification-directory", "subscription-help", "troubleshooting"],
                "tip": "📁 Subscription notifications are automatically saved to files. Use the 'notification-directory' prompt to learn more."
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
                name: "subscribe_record".to_string(),
                description: "Subscribe to real-time updates for a specific record.\n\n\
                    ⚠️  IMPORTANT: You MUST ask the user how many samples to collect before subscribing.\n\
                    Unlimited subscriptions can fill disk space.\n\n\
                    Behavior:\n\
                    - If max_samples is set (e.g., 50): Auto-unsubscribe after N samples\n\
                    - If max_samples is null: Runs indefinitely (requires explicit user confirmation)\n\n\
                    Always suggest appropriate sample limits:\n\
                    - Quick check: 10-30 samples (~20-60 seconds)\n\
                    - Short monitoring: 50-100 samples (~2-3 minutes)\n\
                    - Extended analysis: 200-500 samples (~7-17 minutes)\n\
                    - Continuous: null (only if user explicitly confirms)\n\n\
                    📁 NOTIFICATIONS: Subscription data is automatically saved to JSONL files.\n\
                    The response includes 'notification_file' path. You can also use:\n\
                    - 'get_notification_directory' tool to find where files are stored\n\
                    - 'notification-directory' prompt for detailed file format info\n\
                    - 'subscription-help' prompt for complete subscription guide".to_string(),
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
                        },
                        "max_samples": {
                            "type": ["integer", "null"],
                            "description": "Maximum samples before auto-unsubscribe. \
                                Set to null for unlimited (requires explicit user confirmation). \
                                Examples: 50 for quick check, 200 for analysis, null for continuous.",
                            "minimum": 1
                        }
                    },
                    "required": ["socket_path", "record_name", "max_samples"],
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
                description: "List all active subscriptions with their metadata. \
                    Each subscription includes the 'notification_file' path where data is saved. \
                    Use 'subscription-help' prompt for tips on analyzing subscription data.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "get_notification_directory".to_string(),
                description: "Get the path where subscription notifications are saved. \
                    Notifications are automatically saved as JSONL files when you subscribe to records. \
                    Use this tool to find where the data is stored.".to_string(),
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
            "get_notification_directory" => {
                tools::get_notification_directory(params.arguments).await?
            }
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
                10,   // queue_size
                None, // max_samples (unlimited for resource subscriptions)
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

        let messages = prompts::get_prompt(&params.name, &self.notification_dir)
            .ok_or_else(|| McpError::InvalidParams(format!("Unknown prompt: {}", params.name)))?;

        Ok(PromptsGetResult {
            description: Some(format!("Prompt: {}", params.name)),
            messages,
        })
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new(PathBuf::from("/tmp")).0
    }
}
