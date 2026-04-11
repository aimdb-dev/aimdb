//! AimDB MCP Server
//!
//! Model Context Protocol (MCP) server for AimDB introspection.
//! Enables LLMs (like Claude, Copilot) to discover and interact with
//! running AimDB instances via natural language.

use aimdb_mcp::protocol::{
    InitializeParams, JsonRpcError, JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, PromptsGetParams, ResourceReadParams, ToolCallParams,
};
use aimdb_mcp::{McpServer, StdioTransport};
use clap::Parser;
use serde_json::Value;
use tracing::{debug, error, info, warn};

/// AimDB MCP Server — LLM-powered introspection for AimDB instances.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Public mode: expose only read-only tools (discover_instances, list_records, get_record).
    /// Intended for untrusted / internet-facing endpoints.
    #[arg(long)]
    public: bool,

    /// Default Unix socket path for AimDB connections.
    /// Tools will use this instead of requiring an explicit socket_path argument.
    /// Equivalent to setting the AIMDB_SOCKET environment variable.
    #[arg(long, value_name = "PATH")]
    socket: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Expose --socket as AIMDB_SOCKET so all tool code picks it up automatically
    if let Some(ref socket) = cli.socket {
        std::env::set_var("AIMDB_SOCKET", socket);
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // Log to stderr, stdout is for JSON-RPC
        .init();

    info!("🚀 Starting AimDB MCP Server");
    info!("📡 Protocol: MCP 2025-06-18 over JSON-RPC 2.0");
    info!("🔌 Transport: stdio (NDJSON)");
    if cli.public {
        info!("🔒 Public mode: only read-only tools are available");
    }
    if let Some(ref socket) = cli.socket {
        info!("📎 Default socket: {}", socket);
    }

    // Create server and transport
    let server = McpServer::new().with_public_mode(cli.public);
    let mut transport = StdioTransport::new();

    info!("✅ Server ready, waiting for initialize request...");

    // Main event loop
    loop {
        let line = match transport.read_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                info!("📪 EOF received, shutting down gracefully");
                break;
            }
            Err(e) => {
                error!("❌ Error reading from stdin: {}", e);
                break;
            }
        };

        // Process the request
        if let Err(e) = process_request(&server, &mut transport, line).await {
            error!("❌ Error processing request: {}", e);
            break;
        }
    }

    info!("👋 AimDB MCP Server stopped");
}

/// Process a single JSON-RPC request
async fn process_request(
    server: &McpServer,
    transport: &mut StdioTransport,
    line: String,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!("📨 Received: {}", line);

    // Parse JSON-RPC request
    let request: JsonRpcRequest = match serde_json::from_str(&line) {
        Ok(req) => req,
        Err(e) => {
            error!("❌ Failed to parse JSON-RPC request: {}", e);
            let error_response = JsonRpcErrorResponse::new(
                Value::Null,
                JsonRpcError::new(-32700, "Parse error".to_string(), None),
            );
            if let Ok(response_line) = serde_json::to_string(&error_response) {
                let _ = transport.write_line(&response_line).await;
            }
            return Ok(());
        }
    };

    let request_id = request.id.clone().unwrap_or(Value::Null);
    let method = request.method.clone();

    debug!("🎯 Method: {}, ID: {:?}", method, request_id);

    // Dispatch to handlers
    let response_value = match method.as_str() {
        "initialize" => {
            info!("🔌 Handling initialize request");
            match serde_json::from_value::<InitializeParams>(request.params.unwrap_or(Value::Null))
            {
                Ok(params) => match server.handle_initialize(params).await {
                    Ok(result) => match serde_json::to_value(result) {
                        Ok(v) => {
                            info!("✅ Server initialized successfully");
                            // Send initialized notification
                            let notification = JsonRpcNotification::new(
                                "notifications/initialized".to_string(),
                                None,
                            );
                            if let Ok(notif_line) = serde_json::to_string(&notification) {
                                let _ = transport.write_line(&notif_line).await;
                            }
                            Some(v)
                        }
                        Err(e) => {
                            error!("Failed to serialize result: {}", e);
                            None
                        }
                    },
                    Err(e) => {
                        warn!("Initialize failed: {}", e);
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(e.error_code(), e.message(), None),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                },
                Err(e) => {
                    error!("Invalid initialize params: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, format!("Invalid params: {}", e), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "tools/list" => {
            debug!("🔧 Handling tools/list request");
            match server.handle_tools_list().await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        error!("Failed to serialize tools list: {}", e);
                        None
                    }
                },
                Err(e) => {
                    warn!("tools/list failed: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(e.error_code(), e.message(), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "tools/call" => {
            debug!("🛠️  Handling tools/call request");
            match serde_json::from_value::<ToolCallParams>(request.params.unwrap_or(Value::Null)) {
                Ok(params) => match server.handle_tools_call(params).await {
                    Ok(result) => match serde_json::to_value(result) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            error!("Failed to serialize tool call result: {}", e);
                            None
                        }
                    },
                    Err(e) => {
                        warn!("tools/call failed: {}", e);
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(e.error_code(), e.message(), None),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                },
                Err(e) => {
                    error!("Invalid tools/call params: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, format!("Invalid params: {}", e), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "resources/list" => {
            debug!("📋 Handling resources/list request");
            match server.handle_resources_list().await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        error!("Failed to serialize resources/list result: {}", e);
                        None
                    }
                },
                Err(e) => {
                    warn!("resources/list failed: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(e.error_code(), e.message(), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "resources/read" => {
            debug!("📖 Handling resources/read request");
            match serde_json::from_value::<ResourceReadParams>(
                request.params.unwrap_or(Value::Null),
            ) {
                Ok(params) => match server.handle_resources_read(params).await {
                    Ok(result) => match serde_json::to_value(result) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            error!("Failed to serialize resources/read result: {}", e);
                            None
                        }
                    },
                    Err(e) => {
                        warn!("resources/read failed: {}", e);
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(e.error_code(), e.message(), None),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                },
                Err(e) => {
                    error!("Invalid resources/read params: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, format!("Invalid params: {}", e), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "resources/subscribe" => {
            debug!("🔔 resources/subscribe not supported (use drain_record tool instead)");
            let error_response = JsonRpcErrorResponse::new(
                request_id.clone(),
                JsonRpcError::new(
                    -32601,
                    "Resource subscriptions are not supported. Use the drain_record tool for batch data access.".to_string(),
                    None,
                ),
            );
            if let Ok(response_line) = serde_json::to_string(&error_response) {
                let _ = transport.write_line(&response_line).await;
            }
            return Ok(());
        }
        "resources/unsubscribe" => {
            debug!("🔕 resources/unsubscribe not supported (use drain_record tool instead)");
            let error_response = JsonRpcErrorResponse::new(
                request_id.clone(),
                JsonRpcError::new(
                    -32601,
                    "Resource subscriptions are not supported. Use the drain_record tool for batch data access.".to_string(),
                    None,
                ),
            );
            if let Ok(response_line) = serde_json::to_string(&error_response) {
                let _ = transport.write_line(&response_line).await;
            }
            return Ok(());
        }
        "prompts/list" => {
            debug!("📋 Handling prompts/list request");
            match server.handle_prompts_list().await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        error!("Failed to serialize prompts/list result: {}", e);
                        None
                    }
                },
                Err(e) => {
                    warn!("prompts/list failed: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(e.error_code(), e.message(), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "prompts/get" => {
            debug!("📝 Handling prompts/get request");
            match serde_json::from_value::<PromptsGetParams>(request.params.unwrap_or(Value::Null))
            {
                Ok(params) => match server.handle_prompts_get(params).await {
                    Ok(result) => match serde_json::to_value(result) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            error!("Failed to serialize prompts/get result: {}", e);
                            None
                        }
                    },
                    Err(e) => {
                        warn!("prompts/get failed: {}", e);
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(e.error_code(), e.message(), None),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                },
                Err(e) => {
                    error!("Invalid prompts/get params: {}", e);
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, format!("Invalid params: {}", e), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        _ => {
            warn!("⚠️  Unknown method: {}", method);
            let error_response = JsonRpcErrorResponse::new(
                request_id.clone(),
                JsonRpcError::new(-32601, format!("Method not found: {}", method), None),
            );
            if let Ok(response_line) = serde_json::to_string(&error_response) {
                let _ = transport.write_line(&response_line).await;
            }
            return Ok(());
        }
    };

    // Send response if we have a result
    if let Some(result) = response_value {
        let response = JsonRpcResponse::new(request_id, result);
        match serde_json::to_string(&response) {
            Ok(response_line) => {
                if let Err(e) = transport.write_line(&response_line).await {
                    error!("Failed to write response: {}", e);
                    return Err(e.into());
                }
            }
            Err(e) => {
                error!("Failed to serialize response: {}", e);
            }
        }
    }

    Ok(())
}
