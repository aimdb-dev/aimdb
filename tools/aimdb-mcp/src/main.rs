//! AimDB MCP Server
//!
//! Model Context Protocol (MCP) server for AimDB introspection.
//! Enables LLMs (like Claude, Copilot) to discover and interact with
//! running AimDB instances via natural language.

use aimdb_mcp::protocol::{
    InitializeParams, JsonRpcError, JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, PromptsGetParams, ResourceReadParams, ToolCallParams,
};
use aimdb_mcp::{McpServer, NotificationFileWriter, StdioTransport};
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

#[tokio::main]
async fn main() {
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

    // Configuration from environment
    let notification_files_enabled = std::env::var("AIMDB_MCP_NOTIFICATION_FILES")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .unwrap_or(true);

    let notification_dir = std::env::var("AIMDB_MCP_NOTIFICATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".aimdb-mcp")
                .join("notifications")
        });

    info!(
        "📁 Notification files: {} (dir: {})",
        if notification_files_enabled {
            "enabled"
        } else {
            "disabled"
        },
        notification_dir.display()
    );

    info!(
        "📁 Notification files: {} (dir: {})",
        if notification_files_enabled {
            "enabled"
        } else {
            "disabled"
        },
        notification_dir.display()
    );

    // Create notification file writer
    let notification_writer =
        match NotificationFileWriter::new(notification_dir.clone(), notification_files_enabled)
            .await
        {
            Ok(writer) => writer,
            Err(e) => {
                error!("❌ Failed to initialize notification writer: {}", e);
                error!("   Continuing without notification file support");
                NotificationFileWriter::new(PathBuf::from("/tmp"), false)
                    .await
                    .unwrap()
            }
        };

    // Create server and transport
    let (server, mut notification_rx) = McpServer::new(notification_dir);
    let mut transport = StdioTransport::new();

    info!("✅ Server ready, waiting for initialize request...");

    // Main event loop - use select! to handle both requests and notifications
    loop {
        tokio::select! {
            // Handle incoming requests from stdin
            read_result = transport.read_line() => {
                let line = match read_result {
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

                // Process the request (moved request handling code here)
                if let Err(e) = process_request(&server, &mut transport, line).await {
                    error!("❌ Error processing request: {}", e);
                    break;
                }
            }

            // Handle outgoing notifications
            Some(notification) = notification_rx.recv() => {
                debug!("📢 Forwarding notification: {:?}", notification);

                // Write notification to file (if enabled)
                if let Err(e) = notification_writer.write(&notification).await {
                    error!("⚠️  Failed to write notification to file: {}", e);
                    // Continue processing - don't break on file write errors
                }

                // Forward notification to client
                if let Ok(notification_line) = serde_json::to_string(&notification) {
                    if let Err(e) = transport.write_line(&notification_line).await {
                        error!("❌ Failed to forward notification: {}", e);
                        break;
                    }
                } else {
                    error!("❌ Failed to serialize notification");
                }
            }
        }
    }

    // Flush all notification files before shutdown
    if let Err(e) = notification_writer.flush_all().await {
        error!("⚠️  Failed to flush notification files: {}", e);
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
            debug!("🔔 Handling resources/subscribe request");
            match request.params {
                Some(Value::Object(params)) => {
                    if let Some(Value::String(uri)) = params.get("uri") {
                        match server.handle_resources_subscribe(uri.clone()).await {
                            Ok(subscription_id) => {
                                match serde_json::to_value(
                                    json!({"subscription_id": subscription_id}),
                                ) {
                                    Ok(v) => Some(v),
                                    Err(e) => {
                                        error!(
                                            "Failed to serialize resources/subscribe result: {}",
                                            e
                                        );
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("resources/subscribe failed: {}", e);
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
                    } else {
                        error!("Invalid resources/subscribe params: missing uri");
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(
                                -32602,
                                "Invalid params: missing uri".to_string(),
                                None,
                            ),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                }
                _ => {
                    error!("Invalid resources/subscribe params");
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, "Invalid params".to_string(), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
        }
        "resources/unsubscribe" => {
            debug!("🔕 Handling resources/unsubscribe request");
            match request.params {
                Some(Value::Object(params)) => {
                    if let Some(Value::String(subscription_id)) = params.get("subscription_id") {
                        match server
                            .handle_resources_unsubscribe(subscription_id.clone())
                            .await
                        {
                            Ok(()) => match serde_json::to_value(json!({"success": true})) {
                                Ok(v) => Some(v),
                                Err(e) => {
                                    error!(
                                        "Failed to serialize resources/unsubscribe result: {}",
                                        e
                                    );
                                    None
                                }
                            },
                            Err(e) => {
                                warn!("resources/unsubscribe failed: {}", e);
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
                    } else {
                        error!("Invalid resources/unsubscribe params: missing subscription_id");
                        let error_response = JsonRpcErrorResponse::new(
                            request_id.clone(),
                            JsonRpcError::new(
                                -32602,
                                "Invalid params: missing subscription_id".to_string(),
                                None,
                            ),
                        );
                        if let Ok(response_line) = serde_json::to_string(&error_response) {
                            let _ = transport.write_line(&response_line).await;
                        }
                        return Ok(());
                    }
                }
                _ => {
                    error!("Invalid resources/unsubscribe params");
                    let error_response = JsonRpcErrorResponse::new(
                        request_id.clone(),
                        JsonRpcError::new(-32602, "Invalid params".to_string(), None),
                    );
                    if let Ok(response_line) = serde_json::to_string(&error_response) {
                        let _ = transport.write_line(&response_line).await;
                    }
                    return Ok(());
                }
            }
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
