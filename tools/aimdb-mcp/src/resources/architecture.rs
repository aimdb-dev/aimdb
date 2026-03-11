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
use std::path::PathBuf;
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
pub async fn read_resource(uri: &str) -> McpResult<ResourceReadResult> {
    debug!("architecture read_resource: {uri}");

    let text = match uri {
        // ── diagram & conflicts need the parsed state ──────────────────
        "aimdb://architecture" | "aimdb://architecture/conflicts" => {
            let uri_owned = uri.to_string();
            tokio::task::spawn_blocking(move || read_with_state(&uri_owned))
                .await
                .map_err(|e| McpError::Internal(format!("spawn_blocking join: {e}")))?
                .map_err(|e| McpError::Internal(format!("reading state.toml: {e}")))?
        }

        // ── raw TOML — just return the file bytes ──────────────────────
        "aimdb://architecture/state" => {
            let state_path = default_state_path();
            read_file_or_fallback(&state_path, "# No state.toml found.\n", "state.toml").await?
        }

        // ── conventions — compile-time constant, no I/O ────────────────
        "aimdb://architecture/conventions" => CONVENTIONS.to_string(),

        // ── memory — read .aimdb/memory.md ─────────────────────────────
        "aimdb://architecture/memory" => {
            let memory_path = default_memory_path();
            read_file_or_fallback(
                &memory_path,
                "# AimDB Architecture Memory\n\nNo memory recorded yet. \
                 The architecture agent will populate this after the first \
                 confirmed proposal.\n",
                "memory.md",
            )
            .await?
        }

        _ => {
            return Err(McpError::InvalidParams(format!(
                "Unknown architecture resource: {uri}"
            )))
        }
    };

    let mime_type = Some(
        match uri {
            "aimdb://architecture" => "text/plain",
            "aimdb://architecture/state" => "application/toml",
            "aimdb://architecture/conflicts" => "application/json",
            "aimdb://architecture/conventions" => "text/markdown",
            "aimdb://architecture/memory" => "text/markdown",
            _ => "text/plain",
        }
        .to_string(),
    );

    Ok(ResourceReadResult {
        contents: vec![ResourceContent {
            uri: uri.to_string(),
            mime_type,
            text: Some(text),
            blob: None,
        }],
    })
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Read a file with `tokio::fs`, returning a fallback string when absent.
async fn read_file_or_fallback(path: &PathBuf, fallback: &str, label: &str) -> McpResult<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(contents),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(fallback.to_string()),
        Err(e) => Err(McpError::Internal(format!("reading {label}: {e}"))),
    }
}

/// Synchronous helper executed inside `spawn_blocking`.
/// Reads + parses state.toml and produces the text for `/` or `/conflicts`.
fn read_with_state(uri: &str) -> Result<String, String> {
    let state_path = default_state_path();
    let state_opt = read_state(&state_path).map_err(|e| e.to_string())?;

    match uri {
        "aimdb://architecture" => match state_opt {
            None => {
                Ok("No state.toml found. Run the onboarding prompt to get started.".to_string())
            }
            Some(state) => Ok(aimdb_codegen::generate_mermaid(&state)),
        },

        "aimdb://architecture/conflicts" => match state_opt {
            None => Ok(serde_json::to_string_pretty(&serde_json::json!({
                "errors": [],
                "warnings": [],
                "note": "No state.toml found"
            }))
            .unwrap()),
            Some(state) => {
                let errors = aimdb_codegen::validate(&state);
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
                .map_err(|e| format!("serialising conflicts: {e}"))
            }
        },

        _ => unreachable!("read_with_state called for unexpected URI: {uri}"),
    }
}
