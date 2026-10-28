//! AimDB MCP Server
//!
//! Model Context Protocol (MCP) server for AimDB introspection.
//! Enables LLMs (like Claude, Copilot) to discover and interact with
//! running AimDB instances via natural language.

mod events;

use crate::events::{report, McpEvent};

use aimdb_mcp::protocol::{
    InitializeParams, JsonRpcError, JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, PromptsGetParams, ResourceReadParams, ToolCallParams,
};
use aimdb_mcp::{McpServer, StdioTransport};
use clap::Parser;
use serde_json::Value;
use tracing::{debug, info};

/// AimDB MCP Server — LLM-powered introspection for AimDB instances.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Public mode: expose only read-only tools (discover_instances, list_records, get_record).
    /// Intended for untrusted / internet-facing endpoints.
    #[arg(long)]
    public: bool,

    /// Also offer the tools that change a running instance: set_record,
    /// reset_stage_profiling and reset_buffer_metrics. Off by default so an
    /// agent connected for introspection cannot write to a live system.
    /// Ignored with --public.
    #[arg(long)]
    allow_writes: bool,

    /// Default endpoint for AimDB connections: a `scheme://` URL (`unix://PATH`,
    /// `serial://DEVICE?baud=N`) or a bare path. Tools use this instead of
    /// requiring an explicit `endpoint` argument. Equivalent to setting the
    /// AIMDB_CONNECT environment variable.
    #[arg(long, value_name = "ENDPOINT")]
    connect: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Some(ref endpoint) = cli.connect {
        aimdb_mcp::tools::set_default_endpoint(endpoint.clone());
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
    } else if cli.allow_writes {
        info!("✏️  Write tools enabled (--allow-writes)");
    }
    if let Some(ref endpoint) = cli.connect {
        info!("📎 Default endpoint: {}", endpoint);
    }

    // Create server and transport
    let server = McpServer::new()
        .with_public_mode(cli.public)
        .with_allow_writes(cli.allow_writes);
    let mut transport = StdioTransport::new();

    run_server(&server, &mut transport).await;
}

/// Private I/O seam for testing failures without redirecting process stdio.
trait RequestTransport {
    async fn read_line(&mut self) -> aimdb_mcp::error::McpResult<Option<String>>;
    async fn write_line(&mut self, line: &str) -> aimdb_mcp::error::McpResult<()>;
}
impl RequestTransport for StdioTransport {
    async fn read_line(&mut self) -> aimdb_mcp::error::McpResult<Option<String>> {
        StdioTransport::read_line(self).await
    }
    async fn write_line(&mut self, line: &str) -> aimdb_mcp::error::McpResult<()> {
        StdioTransport::write_line(self, line).await
    }
}
async fn run_server(server: &McpServer, transport: &mut impl RequestTransport) {
    report(McpEvent::Ready);

    // Main event loop
    loop {
        let line = match transport.read_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                report(McpEvent::EndOfInput);
                break;
            }
            Err(e) => {
                report(McpEvent::InputFailed { error: &e });
                break;
            }
        };

        // Process the request
        if let Err(e) = process_request(server, transport, line).await {
            report(McpEvent::RequestProcessingFailed { error: e.as_ref() });
            break;
        }
    }

    report(McpEvent::Stopped);
}

/// Process a single JSON-RPC request
async fn process_request(
    server: &McpServer,
    transport: &mut impl RequestTransport,
    line: String,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!("📨 Received: {}", line);

    // Parse JSON-RPC request
    let request: JsonRpcRequest = match serde_json::from_str(&line) {
        Ok(req) => req,
        Err(e) => {
            report(McpEvent::RequestParseFailed { error: &e });
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

    // A JSON-RPC notification has no `id` and MUST NOT receive a response — e.g.
    // the client's `notifications/initialized` after the handshake. Acknowledge by
    // ignoring it; replying (even with an error) violates the spec and trips strict
    // clients. The server is already Ready post-`initialize`, so none of the
    // notifications we currently receive need server-side handling.
    if request.id.is_none() {
        debug!("Ignoring notification: {}", method);
        return Ok(());
    }

    // Dispatch to handlers
    let response_value = match method.as_str() {
        "initialize" => {
            report(McpEvent::InitializeStarted);
            match serde_json::from_value::<InitializeParams>(request.params.unwrap_or(Value::Null))
            {
                Ok(params) => match server.handle_initialize(params).await {
                    Ok(result) => match serde_json::to_value(result) {
                        Ok(v) => {
                            report(McpEvent::Initialized);
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
                            report(McpEvent::InitializeResultEncodeFailed { error: &e });
                            None
                        }
                    },
                    Err(e) => {
                        report(McpEvent::InitializeFailed { error: &e });
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
                    report(McpEvent::InitializeParamsInvalid { error: &e });
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
                        report(McpEvent::ToolsListEncodeFailed { error: &e });
                        None
                    }
                },
                Err(e) => {
                    report(McpEvent::ToolsListFailed { error: &e });
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
                            report(McpEvent::ToolCallEncodeFailed { error: &e });
                            None
                        }
                    },
                    Err(e) => {
                        report(McpEvent::ToolCallFailed { error: &e });
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
                    report(McpEvent::ToolCallParamsInvalid { error: &e });
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
                        report(McpEvent::ResourcesListEncodeFailed { error: &e });
                        None
                    }
                },
                Err(e) => {
                    report(McpEvent::ResourcesListFailed { error: &e });
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
                            report(McpEvent::ResourceReadEncodeFailed { error: &e });
                            None
                        }
                    },
                    Err(e) => {
                        report(McpEvent::ResourceReadFailed { error: &e });
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
                    report(McpEvent::ResourceReadParamsInvalid { error: &e });
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
                        report(McpEvent::PromptsListEncodeFailed { error: &e });
                        None
                    }
                },
                Err(e) => {
                    report(McpEvent::PromptsListFailed { error: &e });
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
                            report(McpEvent::PromptGetEncodeFailed { error: &e });
                            None
                        }
                    },
                    Err(e) => {
                        report(McpEvent::PromptGetFailed { error: &e });
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
                    report(McpEvent::PromptGetParamsInvalid { error: &e });
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
            report(McpEvent::UnknownMethod { method: &method });
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
                    report(McpEvent::ResponseWriteFailed { error: &e });
                    return Err(e.into());
                }
            }
            Err(e) => {
                report(McpEvent::ResponseEncodeFailed { error: &e });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct FakeTransport {
        input: VecDeque<aimdb_mcp::error::McpResult<Option<String>>>,
        output: Vec<serde_json::Value>,
        fail_write: bool,
    }
    impl RequestTransport for FakeTransport {
        async fn read_line(&mut self) -> aimdb_mcp::error::McpResult<Option<String>> {
            self.input.pop_front().unwrap_or(Ok(None))
        }
        async fn write_line(&mut self, line: &str) -> aimdb_mcp::error::McpResult<()> {
            if self.fail_write {
                return Err(std::io::Error::other("broken writer").into());
            }
            self.output.push(serde_json::from_str(line).unwrap());
            Ok(())
        }
    }
    fn collector() -> (events::capture::Observer, Arc<Mutex<Vec<&'static str>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer = {
            let seen = seen.clone();
            Arc::new(move |event: &McpEvent<'_>| {
                seen.lock().unwrap().push(match event {
                    McpEvent::Ready => "ready",
                    McpEvent::EndOfInput => "eof",
                    McpEvent::Stopped => "stopped",
                    McpEvent::RequestParseFailed { .. } => "parse",
                    McpEvent::UnknownMethod { method } => {
                        assert_eq!(*method, "unknown");
                        "method"
                    }
                    McpEvent::InputFailed { .. } => "input",
                    McpEvent::ResponseWriteFailed { .. } => "write",
                    McpEvent::RequestProcessingFailed { .. } => "processing",
                    other => panic!("unexpected {other:?}"),
                });
            })
        };
        (observer, seen)
    }
    #[tokio::test]
    async fn malformed_and_unknown_requests_keep_their_json_rpc_responses() {
        let server = McpServer::new();
        let mut io = FakeTransport {
            input: VecDeque::from([
                Ok(Some("invalid".into())),
                Ok(Some(
                    r#"{"jsonrpc":"2.0","id":1,"method":"unknown"}"#.into(),
                )),
            ]),
            ..Default::default()
        };
        let (observer, seen) = collector();
        events::capture::future(observer, run_server(&server, &mut io)).await;
        assert_eq!(
            *seen.lock().unwrap(),
            ["ready", "parse", "method", "eof", "stopped"]
        );
        assert_eq!(io.output.len(), 2);
        assert_eq!(io.output[0]["error"]["code"], -32700);
        assert_eq!(io.output[1]["error"]["code"], -32601);
    }
    #[tokio::test]
    async fn input_failure_stops_with_a_typed_event() {
        let mut io = FakeTransport {
            input: VecDeque::from([Err(std::io::Error::other("broken reader").into())]),
            ..Default::default()
        };
        let (observer, seen) = collector();
        events::capture::future(observer, run_server(&McpServer::new(), &mut io)).await;
        assert_eq!(*seen.lock().unwrap(), ["ready", "input", "stopped"]);
        assert!(io.output.is_empty());
    }
    #[tokio::test]
    async fn response_write_failure_preserves_the_error_exit() {
        // initialize produces a successful response through the final write path.
        let mut io = FakeTransport {
            input: VecDeque::from([Ok(Some(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.into(),
            ))]),
            fail_write: true,
            ..Default::default()
        };
        let server = McpServer::new();
        let (observer, seen) = collector();
        // tools/list before initialization uses its existing error-response path,
        // so initialize first without capturing its informational events.
        let mut init = FakeTransport::default();
        process_request(&server, &mut init, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.into()).await.unwrap();
        events::capture::future(observer, run_server(&server, &mut io)).await;
        assert_eq!(
            *seen.lock().unwrap(),
            ["ready", "write", "processing", "stopped"]
        );
    }
}
