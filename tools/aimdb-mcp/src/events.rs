//! Typed operational reporting; payloads live only for the synchronous call.

#[derive(Debug)]
pub(crate) enum McpEvent<'a> {
    Ready,
    EndOfInput,
    Stopped,
    InitializeStarted,
    Initialized,
    InputFailed {
        error: &'a aimdb_mcp::McpError,
    },
    RequestProcessingFailed {
        error: &'a (dyn std::error::Error + 'static),
    },
    RequestParseFailed {
        error: &'a serde_json::Error,
    },
    InitializeResultEncodeFailed {
        error: &'a serde_json::Error,
    },
    InitializeFailed {
        error: &'a aimdb_mcp::McpError,
    },
    InitializeParamsInvalid {
        error: &'a serde_json::Error,
    },
    ToolsListEncodeFailed {
        error: &'a serde_json::Error,
    },
    ToolsListFailed {
        error: &'a aimdb_mcp::McpError,
    },
    ToolCallEncodeFailed {
        error: &'a serde_json::Error,
    },
    ToolCallFailed {
        error: &'a aimdb_mcp::McpError,
    },
    ToolCallParamsInvalid {
        error: &'a serde_json::Error,
    },
    ResourcesListEncodeFailed {
        error: &'a serde_json::Error,
    },
    ResourcesListFailed {
        error: &'a aimdb_mcp::McpError,
    },
    ResourceReadEncodeFailed {
        error: &'a serde_json::Error,
    },
    ResourceReadFailed {
        error: &'a aimdb_mcp::McpError,
    },
    ResourceReadParamsInvalid {
        error: &'a serde_json::Error,
    },
    PromptsListEncodeFailed {
        error: &'a serde_json::Error,
    },
    PromptsListFailed {
        error: &'a aimdb_mcp::McpError,
    },
    PromptGetEncodeFailed {
        error: &'a serde_json::Error,
    },
    PromptGetFailed {
        error: &'a aimdb_mcp::McpError,
    },
    PromptGetParamsInvalid {
        error: &'a serde_json::Error,
    },
    ResponseWriteFailed {
        error: &'a aimdb_mcp::McpError,
    },
    ResponseEncodeFailed {
        error: &'a serde_json::Error,
    },
    UnknownMethod {
        method: &'a str,
    },
}

pub(crate) fn report(event: McpEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        McpEvent::Ready => {
            tracing::info!("✅ Server ready, waiting for initialize request...");
        }
        McpEvent::EndOfInput => {
            tracing::info!("📪 EOF received, shutting down gracefully");
        }
        McpEvent::Stopped => {
            tracing::info!("👋 AimDB MCP Server stopped");
        }
        McpEvent::InitializeStarted => {
            tracing::info!("🔌 Handling initialize request");
        }
        McpEvent::Initialized => {
            tracing::info!("✅ Server initialized successfully");
        }
        McpEvent::InputFailed { error } => {
            tracing::error!("❌ Error reading from stdin: {}", error)
        }
        McpEvent::RequestProcessingFailed { error } => {
            tracing::error!("❌ Error processing request: {}", error)
        }
        McpEvent::RequestParseFailed { error } => {
            tracing::error!("❌ Failed to parse JSON-RPC request: {}", error)
        }
        McpEvent::InitializeResultEncodeFailed { error } => {
            tracing::error!("Failed to serialize result: {}", error)
        }
        McpEvent::InitializeFailed { error } => {
            tracing::warn!("Initialize failed: {}", error);
        }
        McpEvent::InitializeParamsInvalid { error } => {
            tracing::error!("Invalid initialize params: {}", error)
        }
        McpEvent::ToolsListEncodeFailed { error } => {
            tracing::error!("Failed to serialize tools list: {}", error)
        }
        McpEvent::ToolsListFailed { error } => {
            tracing::warn!("tools/list failed: {}", error);
        }
        McpEvent::ToolCallEncodeFailed { error } => {
            tracing::error!("Failed to serialize tool call result: {}", error)
        }
        McpEvent::ToolCallFailed { error } => {
            tracing::warn!("tools/call failed: {}", error);
        }
        McpEvent::ToolCallParamsInvalid { error } => {
            tracing::error!("Invalid tools/call params: {}", error)
        }
        McpEvent::ResourcesListEncodeFailed { error } => {
            tracing::error!("Failed to serialize resources/list result: {}", error)
        }
        McpEvent::ResourcesListFailed { error } => {
            tracing::warn!("resources/list failed: {}", error)
        }
        McpEvent::ResourceReadEncodeFailed { error } => {
            tracing::error!("Failed to serialize resources/read result: {}", error)
        }
        McpEvent::ResourceReadFailed { error } => {
            tracing::warn!("resources/read failed: {}", error)
        }
        McpEvent::ResourceReadParamsInvalid { error } => {
            tracing::error!("Invalid resources/read params: {}", error)
        }
        McpEvent::PromptsListEncodeFailed { error } => {
            tracing::error!("Failed to serialize prompts/list result: {}", error)
        }
        McpEvent::PromptsListFailed { error } => {
            tracing::warn!("prompts/list failed: {}", error);
        }
        McpEvent::PromptGetEncodeFailed { error } => {
            tracing::error!("Failed to serialize prompts/get result: {}", error)
        }
        McpEvent::PromptGetFailed { error } => {
            tracing::warn!("prompts/get failed: {}", error);
        }
        McpEvent::PromptGetParamsInvalid { error } => {
            tracing::error!("Invalid prompts/get params: {}", error)
        }
        McpEvent::ResponseWriteFailed { error } => {
            tracing::error!("Failed to write response: {}", error)
        }
        McpEvent::ResponseEncodeFailed { error } => {
            tracing::error!("Failed to serialize response: {}", error)
        }
        McpEvent::UnknownMethod { method } => {
            tracing::warn!("⚠️  Unknown method: {}", method);
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::McpEvent<'_>);
