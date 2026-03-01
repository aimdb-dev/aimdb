//! Architecture agent MCP tools (M11)
//!
//! Provides typed proposal tools for the architecture agent ideation loop:
//! `get_architecture`, `propose_add_record`, `propose_modify_buffer`,
//! `propose_add_connector`, `propose_modify_fields`, `propose_modify_key_variants`,
//! `propose_add_task`, `propose_add_binary`,
//! `resolve_proposal`, `remove_record`, `rename_record`, `remove_task`, `remove_binary`,
//! `validate_against_instance`, `get_buffer_metrics`, `reset_session`.
//!
//! All proposal-related tools are routed through the session state machine,
//! which enforces: `Idle → Gathering → Proposing → (resolve) → Gathering`.

use crate::architecture::conflicts::{detect_conflicts, LiveRecord};
use crate::architecture::session::next_proposal_id;
use crate::architecture::{
    self, apply_change, default_memory_path, default_mermaid_path, default_rust_path,
    default_state_path, ensure_state_initialised, session_store, write_artefacts,
    write_state_locked, Proposal, ProposalResolution, ProposedChange,
};
use crate::error::{McpError, McpResult};
use aimdb_client::AimxClient;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

// ── get_architecture ──────────────────────────────────────────────────────────

/// Return the current architecture state as structured JSON.
///
/// Side-effect: creates a session if none exists, transitions Idle → Gathering.
pub async fn get_architecture(args: Option<Value>) -> McpResult<Value> {
    debug!("get_architecture called");

    #[derive(Debug, Deserialize, Default)]
    struct Params {
        #[serde(default)]
        state_path: Option<String>,
    }
    let params: Params = parse_optional(args)?;
    let path = params
        .state_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_state_path);

    // Transition: Idle → Gathering (or no-op if already in session)
    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;
    let session_info = {
        let mut locked = store
            .lock()
            .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
        let session = locked.on_get_architecture();
        serde_json::to_value(session).unwrap_or_default()
    };

    match architecture::read_state(&path).map_err(|e| McpError::Internal(e.to_string()))? {
        None => Ok(serde_json::json!({
            "exists": false,
            "message": format!(
                "No state.toml found at {}. Start an architecture session with the onboarding prompt.",
                path.display()
            ),
            "session": session_info,
        })),
        Some(state) => {
            let errors = aimdb_codegen::validate(&state);
            let result = serde_json::json!({
                "exists": true,
                "state": serde_json::to_value(&state)?,
                "record_count": state.records.len(),
                "decision_count": state.decisions.len(),
                "validation_errors": errors.iter().filter(|e| e.severity == aimdb_codegen::Severity::Error).count(),
                "validation_warnings": errors.iter().filter(|e| e.severity == aimdb_codegen::Severity::Warning).count(),
                "session": session_info,
            });
            Ok(result)
        }
    }
}

// ── propose_* typed tools ─────────────────────────────────────────────────────
//
// Each tool has a concrete, fully-specified JSON Schema exposed to the MCP
// client so AI agents never have to guess payload structure. Error messages
// include the expected schema as a self-describing hint.

// Shared helper: push a Proposal into the session store and return its ID.
fn submit_proposal(proposal: Proposal) -> McpResult<String> {
    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;
    let mut locked = store
        .lock()
        .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
    let p = locked.on_propose(proposal).map_err(|e| {
        McpError::InvalidParams(format!("Cannot propose in current session state: {e}"))
    })?;
    Ok(p.id.clone())
}

// ── propose_add_record ────────────────────────────────────────────────────────

/// Add a new record. All fields are fully typed — no payload guessing required.
pub async fn propose_add_record(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_add_record called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  name        : string  — PascalCase record name (required)\n",
        "  buffer      : string  — \"SpmcRing\" | \"SingleLatest\" | \"Mailbox\" (required)\n",
        "  capacity    : number  — required when buffer = \"SpmcRing\"\n",
        "  description : string  — human-readable description shown to the user (required)\n",
        "  key_prefix  : string  — common key prefix, e.g. \"sensors.temp.\" (optional, default \"\")\n",
        "  key_variants: string[] — concrete variant names, e.g. [\"Default\"] (optional, default [])\n",
        "  producers   : string[] — task names that write to this record (optional)\n",
        "  consumers   : string[] — task names that read from this record (optional)\n",
        "  fields      : [{name, type, description}][] — value struct fields (optional)\n",
        "  connectors  : [{protocol, direction, url}][] — connector wiring (optional)"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        description: String,
        #[serde(flatten)]
        record: aimdb_codegen::RecordDef,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_add_record: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "add_record".to_string(),
        description: p.description,
        change: ProposedChange::AddRecord { record: p.record },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_modify_buffer ─────────────────────────────────────────────────────

/// Change the buffer type (and optionally capacity) of an existing record.
pub async fn propose_modify_buffer(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_modify_buffer called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  record_name : string — PascalCase name of existing record (required)\n",
        "  description : string — human-readable description shown to the user (required)\n",
        "  buffer      : string — \"SpmcRing\" | \"SingleLatest\" | \"Mailbox\" (required)\n",
        "  capacity    : number — required when buffer = \"SpmcRing\""
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        record_name: String,
        description: String,
        buffer: aimdb_codegen::BufferType,
        capacity: Option<usize>,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_modify_buffer: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "modify_buffer".to_string(),
        description: p.description,
        change: ProposedChange::ModifyBuffer {
            record_name: p.record_name,
            buffer: p.buffer,
            capacity: p.capacity,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_add_connector ─────────────────────────────────────────────────────

/// Add a connector (MQTT, KNX, etc.) to an existing record.
pub async fn propose_add_connector(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_add_connector called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  record_name : string — PascalCase name of existing record (required)\n",
        "  description : string — human-readable description shown to the user (required)\n",
        "  protocol    : string — e.g. \"mqtt\", \"knx\" (required)\n",
        "  direction   : string — \"inbound\" | \"outbound\" (required)\n",
        "  url         : string — topic / address template; may contain {variant} placeholder (required)"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        record_name: String,
        description: String,
        protocol: String,
        direction: aimdb_codegen::ConnectorDirection,
        url: String,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_add_connector: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "add_connector".to_string(),
        description: p.description,
        change: ProposedChange::AddConnector {
            record_name: p.record_name,
            connector: aimdb_codegen::ConnectorDef {
                protocol: p.protocol,
                direction: p.direction,
                url: p.url,
            },
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_modify_fields ─────────────────────────────────────────────────────

/// Replace the fields of an existing record's value struct.
pub async fn propose_modify_fields(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_modify_fields called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  record_name : string — PascalCase name of existing record (required)\n",
        "  description : string — human-readable description shown to the user (required)\n",
        "  fields      : array  — replacement field list (required)\n",
        "    Each element: { \"name\": string, \"type\": \"f64|f32|u8|u16|u32|u64|i8|i16|i32|i64|bool|String\", \"description\": string }"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        record_name: String,
        description: String,
        fields: Vec<aimdb_codegen::FieldDef>,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_modify_fields: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "modify_fields".to_string(),
        description: p.description,
        change: ProposedChange::ModifyFields {
            record_name: p.record_name,
            fields: p.fields,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_modify_key_variants ───────────────────────────────────────────────

/// Set the key variants (and optionally key_prefix) of an existing record.
///
/// Use this after adding a record with no variants, or when adding new devices
/// to an existing key family.
pub async fn propose_modify_key_variants(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_modify_key_variants called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  record_name  : string   — PascalCase name of existing record (required)\n",
        "  description  : string   — human-readable description shown to the user (required)\n",
        "  key_variants : string[] — concrete PascalCase variant names, e.g. [\"Default\"] or\n",
        "                           [\"ApiServer\", \"Worker\", \"Db\"] (required)\n",
        "  key_prefix   : string   — optional common prefix, e.g. \"sensors.temp.\" (optional)"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        record_name: String,
        description: String,
        key_variants: Vec<String>,
        key_prefix: Option<String>,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_modify_key_variants: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "modify_key_variants".to_string(),
        description: p.description,
        change: ProposedChange::ModifyKeyVariants {
            record_name: p.record_name,
            key_variants: p.key_variants,
            key_prefix: p.key_prefix,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_add_task ─────────────────────────────────────────────────────────

/// Add a new task definition. Tasks are async functions that source, transform,
/// or tap data in records.
pub async fn propose_add_task(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_add_task called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  name        : string  — snake_case task function name (required)\n",
        "  description : string  — human-readable description (required)\n",
        "  task_type   : string  — \"source\" | \"transform\" | \"tap\" | \"agent\" (default: \"transform\")\n",
        "  inputs      : [{record, variants?}][] — records this task reads from (optional)\n",
        "  outputs     : [{record, variants?}][] — records this task writes to (optional)"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        description: String,
        #[serde(flatten)]
        task: aimdb_codegen::TaskDef,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("propose_add_task: {e}\n\n{SCHEMA_HINT}")))?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "add_task".to_string(),
        description: p.description,
        change: ProposedChange::AddTask { task: p.task },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── propose_add_binary ──────────────────────────────────────────────────────

/// Add a new binary definition. Binaries are deployable crates that group
/// tasks and optionally declare external broker connections.
pub async fn propose_add_binary(args: Option<Value>) -> McpResult<Value> {
    debug!("propose_add_binary called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  name                : string  — crate directory name (required)\n",
        "  description         : string  — human-readable description (required)\n",
        "  tasks               : string[] — task names belonging to this binary (optional)\n",
        "  external_connectors : [{protocol, env_var, default}][] — broker connections (optional)"
    );

    #[derive(Debug, Deserialize)]
    struct Params {
        description: String,
        #[serde(flatten)]
        binary: aimdb_codegen::BinaryDef,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null)).map_err(|e| {
        McpError::InvalidParams(format!("propose_add_binary: {e}\n\n{SCHEMA_HINT}"))
    })?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "add_binary".to_string(),
        description: p.description,
        change: ProposedChange::AddBinary { binary: p.binary },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "message": "Proposal created. Present it to the user and call resolve_proposal with their decision.",
    }))
}

// ── remove_task ─────────────────────────────────────────────────────────────

/// Propose removal of an existing task (creates a pending proposal).
///
/// Enforces session phase: must be in `Gathering` phase.
pub async fn remove_task(args: Option<Value>) -> McpResult<Value> {
    debug!("remove_task called");

    #[derive(Debug, Deserialize)]
    struct Params {
        task_name: String,
    }

    let params: Params = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("remove_task: {e}")))?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "remove_task".to_string(),
        description: format!("Remove task '{}'", params.task_name),
        change: ProposedChange::RemoveTask {
            task_name: params.task_name.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "task_name": params.task_name,
        "warning": "Removing this task will affect any binaries that reference it and any records listing it as a source/consumer.",
        "message": "Removal proposal created. Present to the user, then call resolve_proposal.",
    }))
}

// ── remove_binary ───────────────────────────────────────────────────────────

/// Propose removal of an existing binary (creates a pending proposal).
///
/// Enforces session phase: must be in `Gathering` phase.
pub async fn remove_binary(args: Option<Value>) -> McpResult<Value> {
    debug!("remove_binary called");

    #[derive(Debug, Deserialize)]
    struct Params {
        binary_name: String,
    }

    let params: Params = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("remove_binary: {e}")))?;

    let proposal_id = submit_proposal(Proposal {
        id: next_proposal_id(),
        change_type: "remove_binary".to_string(),
        description: format!("Remove binary '{}'", params.binary_name),
        change: ProposedChange::RemoveBinary {
            binary_name: params.binary_name.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    Ok(serde_json::json!({
        "proposal_id": proposal_id,
        "binary_name": params.binary_name,
        "warning": "Removing this binary will delete the generated crate scaffold. The task definitions themselves are preserved.",
        "message": "Removal proposal created. Present to the user, then call resolve_proposal.",
    }))
}

// ── resolve_proposal ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ResolveProposalParams {
    proposal_id: String,
    resolution: ProposalResolution,
    /// Optional redirect message when resolution is `revise`
    #[serde(default)]
    redirect: Option<String>,
    #[serde(default)]
    state_path: Option<String>,
    #[serde(default)]
    mermaid_path: Option<String>,
    #[serde(default)]
    rust_path: Option<String>,
}

/// Accept, reject, or redirect (revise) a pending proposal.
///
/// Enforces session phase: must be in `Proposing` phase with a matching
/// proposal ID. On resolve, transitions back to `Gathering`.
pub async fn resolve_proposal(args: Option<Value>) -> McpResult<Value> {
    debug!("resolve_proposal called");
    let params: ResolveProposalParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("resolve_proposal: {e}")))?;

    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;

    // Extract the proposal from the session (validates phase + ID)
    let proposal = {
        let mut locked = store
            .lock()
            .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
        locked
            .on_resolve(&params.proposal_id)
            .map_err(|e| McpError::InvalidParams(format!("Cannot resolve proposal: {e}")))?
    };

    match params.resolution {
        ProposalResolution::Reject => Ok(serde_json::json!({
            "resolution": "rejected",
            "proposal_id": proposal.id,
            "message": "Proposal rejected. No changes were made to state.toml.",
        })),

        ProposalResolution::Revise => Ok(serde_json::json!({
            "resolution": "revise",
            "proposal_id": proposal.id,
            "redirect": params.redirect,
            "message": "Proposal marked for revision. Please revise and call the appropriate propose_* tool again.",
        })),

        ProposalResolution::Confirm => {
            let state_path = params
                .state_path
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_state_path);
            let mermaid_path = params
                .mermaid_path
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_mermaid_path);
            let rust_path = params
                .rust_path
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_rust_path);

            // Read current state (or initialise)
            let mut state = ensure_state_initialised(&state_path)
                .map_err(|e| McpError::Internal(format!("reading state.toml: {e}")))?;

            // Apply the change
            apply_change(&mut state, &proposal.change)
                .map_err(|e| McpError::Internal(format!("applying change: {e}")))?;

            // Validate result before writing
            let errors = aimdb_codegen::validate(&state);
            let blocking_errors: Vec<_> = errors
                .iter()
                .filter(|e| e.severity == aimdb_codegen::Severity::Error)
                .collect();
            if !blocking_errors.is_empty() {
                let msgs: Vec<String> = blocking_errors.iter().map(|e| e.to_string()).collect();
                return Err(McpError::InvalidParams(format!(
                    "Applying this change produces validation errors — cannot confirm:\n{}",
                    msgs.join("\n")
                )));
            }

            // Write state.toml with file lock
            write_state_locked(&state_path, &state)
                .map_err(|e| McpError::Internal(format!("writing state.toml: {e}")))?;

            // Generate artefacts
            let generated = write_artefacts(&state, &mermaid_path, &rust_path)
                .map_err(|e| McpError::Internal(format!("generating artefacts: {e}")))?;

            Ok(serde_json::json!({
                "resolution": "confirmed",
                "proposal_id": proposal.id,
                "change_type": proposal.change_type,
                "state_toml": state_path.display().to_string(),
                "generated": generated,
                "record_count": state.records.len(),
                "message": "Proposal confirmed. state.toml updated, Mermaid and Rust generated.",
            }))
        }
    }
}

// ── save_memory ───────────────────────────────────────────────────────────────

/// Persist ideation context to `.aimdb/memory.md`.
///
/// Call this after every confirmed proposal to record the conversational
/// rationale that led to each decision — the *why* that state.toml cannot
/// express.
pub async fn save_memory(args: Option<Value>) -> McpResult<Value> {
    debug!("save_memory called");

    const SCHEMA_HINT: &str = concat!(
        "Expected fields:\n",
        "  entry       : string — markdown text to append (required)\n",
        "  mode        : \"append\" (default) | \"overwrite\" — append adds a dated\n",
        "                section; overwrite replaces the entire file\n",
        "  memory_path : string — override path (default: .aimdb/memory.md)"
    );

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum MemoryMode {
        #[default]
        Append,
        Overwrite,
    }

    #[derive(Debug, Deserialize)]
    struct Params {
        entry: String,
        #[serde(default)]
        mode: MemoryMode,
        #[serde(default)]
        memory_path: Option<String>,
    }

    let p: Params = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("save_memory: {e}\n\n{SCHEMA_HINT}")))?;

    let path = p
        .memory_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_memory_path);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| McpError::Internal(format!("creating .aimdb dir: {e}")))?;
    }

    let content = match p.mode {
        MemoryMode::Overwrite => p.entry.clone(),
        MemoryMode::Append => {
            let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
            let section = format!("\n---\n<!-- {} -->\n{}\n", timestamp, p.entry.trim());
            if path.exists() {
                let existing = std::fs::read_to_string(&path)
                    .map_err(|e| McpError::Internal(format!("reading memory.md: {e}")))?;
                format!("{}{}", existing.trim_end(), section)
            } else {
                // First write: add a header
                "# AimDB Architecture Memory\n\
                 > Generated by the architecture agent. Do not edit — use the agent to update.\n\
                 > Records the ideation context and design rationale behind each decision.\n"
                    .to_string()
                    + &section
            }
        }
    };

    std::fs::write(&path, &content)
        .map_err(|e| McpError::Internal(format!("writing memory.md: {e}")))?;

    Ok(serde_json::json!({
        "written": path.display().to_string(),
        "mode": match p.mode { MemoryMode::Append => "append", MemoryMode::Overwrite => "overwrite" },
        "bytes": content.len(),
        "message": "Memory updated. The agent will read this on next session start.",
    }))
}

#[derive(Debug, Deserialize)]
struct RemoveRecordParams {
    record_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    state_path: Option<String>,
}

/// Propose removal of an existing record (creates a pending proposal).
///
/// Enforces session phase: must be in `Gathering` phase.
pub async fn remove_record(args: Option<Value>) -> McpResult<Value> {
    debug!("remove_record called");
    let params: RemoveRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("remove_record: {e}")))?;

    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;

    let proposal = Proposal {
        id: next_proposal_id(),
        change_type: "remove_record".to_string(),
        description: format!("Remove record '{}'", params.record_name),
        change: ProposedChange::RemoveRecord {
            record_name: params.record_name.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let id = {
        let mut locked = store
            .lock()
            .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
        let p = locked
            .on_propose(proposal)
            .map_err(|e| McpError::InvalidParams(format!("Cannot propose removal: {e}")))?;
        p.id.clone()
    };

    Ok(serde_json::json!({
        "proposal_id": id,
        "record_name": params.record_name,
        "warning": format!(
            "Removing '{}' will delete its generated key enum and value struct from \
             src/generated_schema.rs. Application code referencing these types will fail to compile.",
            params.record_name
        ),
        "message": "Removal proposal created. Present to the user, then call resolve_proposal.",
    }))
}

// ── rename_record ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RenameRecordParams {
    old_name: String,
    new_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    state_path: Option<String>,
}

/// Propose renaming an existing record (creates a pending proposal).
///
/// Enforces session phase: must be in `Gathering` phase.
pub async fn rename_record(args: Option<Value>) -> McpResult<Value> {
    debug!("rename_record called");
    let params: RenameRecordParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("rename_record: {e}")))?;

    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;

    let proposal = Proposal {
        id: next_proposal_id(),
        change_type: "rename_record".to_string(),
        description: format!("Rename '{}' → '{}'", params.old_name, params.new_name),
        change: ProposedChange::RenameRecord {
            old_name: params.old_name.clone(),
            new_name: params.new_name.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let id = {
        let mut locked = store
            .lock()
            .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
        let p = locked
            .on_propose(proposal)
            .map_err(|e| McpError::InvalidParams(format!("Cannot propose rename: {e}")))?;
        p.id.clone()
    };

    Ok(serde_json::json!({
        "proposal_id": id,
        "old_name": params.old_name,
        "new_name": params.new_name,
        "warning": format!(
            "Renaming '{}' to '{}' will update the generated key enum and value struct names. \
             Application code using the old name ('{}Key', '{}Value') will fail to compile.",
            params.old_name, params.new_name, params.old_name, params.old_name
        ),
        "message": "Rename proposal created. Present to the user, then call resolve_proposal.",
    }))
}

// ── validate_against_instance ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ValidateInstanceParams {
    socket_path: String,
    #[serde(default)]
    state_path: Option<String>,
}

/// Compare state.toml against a live AimDB instance and return conflicts.
///
/// Session-agnostic: works in any phase.
pub async fn validate_against_instance(args: Option<Value>) -> McpResult<Value> {
    debug!("validate_against_instance called");
    let params: ValidateInstanceParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("validate_against_instance: {e}")))?;

    let state_path = params
        .state_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_state_path);

    let state = architecture::read_state(&state_path)
        .map_err(|e| McpError::Internal(e.to_string()))?
        .ok_or_else(|| {
            McpError::InvalidParams(format!(
                "No state.toml at {}. Nothing to validate against.",
                state_path.display()
            ))
        })?;

    // Connect and list live records
    let mut client = AimxClient::connect(&params.socket_path)
        .await
        .map_err(McpError::Client)?;

    let raw = client.list_records().await.map_err(McpError::Client)?;

    // Parse the live records into our lightweight struct
    let live_records: Vec<LiveRecord> = raw
        .into_iter()
        .map(|r| LiveRecord {
            name: r.name,
            buffer_type: r.buffer_type,
            buffer_capacity: r.buffer_capacity,
        })
        .collect();

    let report = detect_conflicts(&state, &live_records);

    Ok(serde_json::to_value(&report)?)
}

// ── get_buffer_metrics ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GetBufferMetricsParams {
    socket_path: String,
    record_key: String,
}

/// Get live buffer metrics for a record key (delegates to list_records, filters by key).
///
/// Session-agnostic: works in any phase.
pub async fn get_buffer_metrics(args: Option<Value>) -> McpResult<Value> {
    debug!("get_buffer_metrics called");
    let params: GetBufferMetricsParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("get_buffer_metrics: {e}")))?;

    let mut client = AimxClient::connect(&params.socket_path)
        .await
        .map_err(McpError::Client)?;

    let raw = client.list_records().await.map_err(McpError::Client)?;

    let matching: Vec<_> = raw
        .into_iter()
        .filter(|r| r.name.contains(&params.record_key))
        .collect();

    if matching.is_empty() {
        return Ok(serde_json::json!({
            "found": false,
            "record_key": params.record_key,
            "message": "No records matching this key were found in the running instance.",
        }));
    }

    Ok(serde_json::json!({
        "found": true,
        "record_key": params.record_key,
        "records": serde_json::to_value(matching)?,
    }))
}

// ── reset_session ─────────────────────────────────────────────────────────────

/// Reset the architecture session, discarding any pending proposals.
///
/// Use this when the user wants to start over or abandon the current
/// ideation cycle.
pub async fn reset_session(_args: Option<Value>) -> McpResult<Value> {
    debug!("reset_session called");

    let store = session_store()
        .ok_or_else(|| McpError::Internal("Session store not initialised".to_string()))?;

    let mut locked = store
        .lock()
        .map_err(|e| McpError::Internal(format!("session store poisoned: {e}")))?;
    locked.reset();

    Ok(serde_json::json!({
        "message": "Session reset. Call get_architecture to begin a new ideation cycle.",
    }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_optional<T: serde::de::DeserializeOwned + Default>(args: Option<Value>) -> McpResult<T> {
    match args {
        None | Some(Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v).map_err(|e| McpError::InvalidParams(e.to_string())),
    }
}
