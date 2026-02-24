//! Architecture agent MCP resources (M11)
//!
//! Exposes 5 resources:
//! - `aimdb://architecture`              → Mermaid diagram (from state.toml)
//! - `aimdb://architecture/state`        → raw state.toml as TOML text
//! - `aimdb://architecture/conflicts`    → validation errors/warnings as JSON
//! - `aimdb://architecture/memory`       → ideation context and design rationale
//! - `aimdb://architecture/conventions`  → Mermaid diagram conventions (embedded, read-only)

use crate::architecture::{default_memory_path, default_state_path, read_state};
use crate::error::{McpError, McpResult};
use crate::protocol::{Resource, ResourceContent, ResourceReadResult};
use tracing::debug;

/// Mermaid diagram conventions — embedded at compile time so the binary is
/// self-contained and users cannot accidentally modify the spec.
const CONVENTIONS: &str = include_str!("../../assets/CONVENTIONS.md");

// ── list ──────────────────────────────────────────────────────────────────────

/// Return the 6 architecture resource descriptors.
pub fn list_resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: "aimdb://architecture".to_string(),
            name: "Architecture Diagram".to_string(),
            description: Some(
                "Mermaid flowchart generated from .aimdb/state.toml. \
                 Reflects the current proposed architecture."
                    .to_string(),
            ),
            mime_type: Some("text/plain".to_string()),
        },
        Resource {
            uri: "aimdb://architecture/state".to_string(),
            name: "Architecture State".to_string(),
            description: Some(
                "Raw .aimdb/state.toml as a TOML document. \
                 Contains all record definitions and connector config."
                    .to_string(),
            ),
            mime_type: Some("application/toml".to_string()),
        },
        Resource {
            uri: "aimdb://architecture/conflicts".to_string(),
            name: "Architecture Validation".to_string(),
            description: Some(
                "Validation errors and warnings from .aimdb/state.toml. \
                 Does not require a running instance."
                    .to_string(),
            ),
            mime_type: Some("application/json".to_string()),
        },
        Resource {
            uri: "aimdb://architecture/conventions".to_string(),
            name: "Mermaid Diagram Conventions".to_string(),
            description: Some(
                "The canonical visual language for AimDB architecture diagrams: node shapes \
                 per buffer type, arrow styles for data flow vs connector metadata, and \
                 labelling rules. Embedded in the binary — read-only."
                    .to_string(),
            ),
            mime_type: Some("text/markdown".to_string()),
        },
        Resource {
            uri: "aimdb://architecture/memory".to_string(),
            name: "Architecture Memory".to_string(),
            description: Some(
                "Ideation context and design rationale from .aimdb/memory.md. \
                 Captures the conversational why behind every decision: questions asked, \
                 answers received, alternatives rejected, and future considerations. \
                 Read this on session start to restore full design context."
                    .to_string(),
            ),
            mime_type: Some("text/markdown".to_string()),
        },
    ]
}

// ── read ──────────────────────────────────────────────────────────────────────

/// Read a single architecture resource by URI.
pub fn read_resource(uri: &str) -> McpResult<ResourceReadResult> {
    debug!("architecture read_resource: {uri}");

    let state_path = default_state_path();

    let state_opt = read_state(&state_path)
        .map_err(|e| McpError::Internal(format!("reading state.toml: {e}")))?;

    let text = match uri {
        "aimdb://architecture" => match &state_opt {
            None => "No state.toml found. Run the onboarding prompt to get started.".to_string(),
            Some(state) => aimdb_codegen::generate_mermaid(state),
        },

        "aimdb://architecture/state" => match &state_opt {
            None => "# No state.toml found.\n".to_string(),
            Some(state) => serde_json::to_string_pretty(state)
                .map_err(|e| McpError::Internal(format!("serialising state: {e}")))?,
        },

        "aimdb://architecture/conflicts" => match &state_opt {
            None => serde_json::to_string_pretty(&serde_json::json!({
                "errors": [],
                "warnings": [],
                "note": "No state.toml found"
            }))
            .unwrap(),
            Some(state) => {
                let errors = aimdb_codegen::validate(state);
                serde_json::to_string_pretty(&serde_json::json!({
                    "errors": errors.iter()
                        .filter(|e| e.severity == aimdb_codegen::Severity::Error)
                        .map(|e| serde_json::json!({ "location": e.location, "message": e.message }))
                        .collect::<Vec<_>>(),
                    "warnings": errors.iter()
                        .filter(|e| e.severity == aimdb_codegen::Severity::Warning)
                        .map(|e| serde_json::json!({ "location": e.location, "message": e.message }))
                        .collect::<Vec<_>>(),
                }))
                .map_err(|e| McpError::Internal(format!("serialising conflicts: {e}")))?
            }
        },

        "aimdb://architecture/conventions" => CONVENTIONS.to_string(),

        "aimdb://architecture/memory" => {
            let memory_path = default_memory_path();
            if memory_path.exists() {
                std::fs::read_to_string(&memory_path)
                    .map_err(|e| McpError::Internal(format!("reading memory.md: {e}")))?
            } else {
                "# AimDB Architecture Memory\n\nNo memory recorded yet. \
                 The architecture agent will populate this after the first confirmed proposal.\n"
                    .to_string()
            }
        }

        _ => {
            return Err(McpError::InvalidParams(format!(
                "Unknown architecture resource: {uri}"
            )))
        }
    };

    let mime_type = if uri.ends_with("architecture") {
        Some("text/plain".to_string())
    } else {
        Some("application/json".to_string())
    };

    Ok(ResourceReadResult {
        contents: vec![ResourceContent {
            uri: uri.to_string(),
            mime_type,
            text: Some(text),
            blob: None,
        }],
    })
}
