//! Architecture agent shared state
//!
//! Manages sessions and provides helpers for reading/writing
//! `.aimdb/state.toml` and running the codegen pipeline.
//!
//! The session state machine enforces the ideation loop:
//! `Idle → Gathering → Proposing → (resolve) → Gathering → ...`

pub mod conflicts;
pub mod session;

use aimdb_codegen::{generate_mermaid, generate_rust, ArchitectureState, RecordDef};
use chrono::Utc;
use fs2::FileExt;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use session::SessionStore;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

// ── Session store (module-level global, same pattern as CONNECTION_POOL) ──────

static SESSION_STORE: OnceCell<Arc<Mutex<SessionStore>>> = OnceCell::new();

/// Initialise the session store (called once at server startup).
pub fn init_session_store() {
    SESSION_STORE
        .set(Arc::new(Mutex::new(SessionStore::default())))
        .ok();
}

/// Access the global session store.
pub fn session_store() -> Option<Arc<Mutex<SessionStore>>> {
    SESSION_STORE.get().cloned()
}

// ── Default paths ─────────────────────────────────────────────────────────────

pub fn default_state_path() -> PathBuf {
    PathBuf::from(".aimdb/state.toml")
}

pub fn default_mermaid_path() -> PathBuf {
    PathBuf::from(".aimdb/architecture.mermaid")
}

pub fn default_rust_path() -> PathBuf {
    PathBuf::from("src/generated_schema.rs")
}

pub fn default_memory_path() -> PathBuf {
    PathBuf::from(".aimdb/memory.md")
}

// ── State I/O ─────────────────────────────────────────────────────────────────

/// Read `.aimdb/state.toml` from the given path (or default).
///
/// Returns `Ok(None)` when the file does not exist yet.
pub fn read_state(path: &Path) -> anyhow::Result<Option<ArchitectureState>> {
    if !path.exists() {
        return Ok(None);
    }
    let src = std::fs::read_to_string(path)?;
    let state = ArchitectureState::from_toml(&src)
        .map_err(|e| anyhow::anyhow!("parse error in {}: {}", path.display(), e))?;
    Ok(Some(state))
}

/// Write state to disk at `path`, creating parent directories as needed.
pub fn write_state(path: &Path, state: &ArchitectureState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml = state.to_toml()?;
    std::fs::write(path, toml)?;
    Ok(())
}

/// Write state to disk with an exclusive file lock, preventing concurrent
/// writes from corrupting state.toml.
pub fn write_state_locked(path: &Path, state: &ArchitectureState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml = state.to_toml()?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.lock_exclusive()?;
    std::io::Write::write_all(&mut &file, toml.as_bytes())?;
    file.unlock()?;
    Ok(())
}

/// Write Mermaid and Rust artefacts derived from `state`.
pub fn write_artefacts(
    state: &ArchitectureState,
    mermaid_path: &Path,
    rust_path: &Path,
) -> anyhow::Result<GeneratedFiles> {
    let mermaid = generate_mermaid(state);
    let rust = generate_rust(state);

    if let Some(p) = mermaid_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    if let Some(p) = rust_path.parent() {
        std::fs::create_dir_all(p)?;
    }

    std::fs::write(mermaid_path, &mermaid)?;
    std::fs::write(rust_path, &rust)?;

    Ok(GeneratedFiles {
        mermaid_path: mermaid_path.display().to_string(),
        rust_path: rust_path.display().to_string(),
        mermaid_lines: mermaid.lines().count(),
        rust_lines: rust.lines().count(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFiles {
    pub mermaid_path: String,
    pub rust_path: String,
    pub mermaid_lines: usize,
    pub rust_lines: usize,
}

// ── Proposal types ────────────────────────────────────────────────────────────

/// A pending architectural change awaiting human confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub change_type: String,
    pub description: String,
    pub change: ProposedChange,
    pub created_at: String,
}

/// The kinds of architectural change the agent can propose.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposedChange {
    /// Add a brand-new record to state.toml
    AddRecord { record: RecordDef },
    /// Change the buffer type (and optionally capacity) of an existing record
    ModifyBuffer {
        record_name: String,
        buffer: aimdb_codegen::BufferType,
        capacity: Option<usize>,
    },
    /// Add a connector to an existing record
    AddConnector {
        record_name: String,
        connector: aimdb_codegen::ConnectorDef,
    },
    /// Replace the fields of an existing record's value struct
    ModifyFields {
        record_name: String,
        fields: Vec<aimdb_codegen::FieldDef>,
    },
    /// Remove an existing record (cascades through Mermaid and codegen)
    RemoveRecord { record_name: String },
    /// Rename an existing record (updates all references)
    RenameRecord { old_name: String, new_name: String },
    /// Replace the key variants (and optionally key_prefix) of an existing record
    ModifyKeyVariants {
        record_name: String,
        key_variants: Vec<String>,
        key_prefix: Option<String>,
    },
}

/// Resolution for a proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalResolution {
    Confirm,
    Reject,
    Revise,
}

// ── Change application ────────────────────────────────────────────────────────

/// Apply a confirmed `ProposedChange` to the given state, updating `last_modified`.
pub fn apply_change(state: &mut ArchitectureState, change: &ProposedChange) -> anyhow::Result<()> {
    state.meta.last_modified = Utc::now().to_rfc3339();

    match change {
        ProposedChange::AddRecord { record } => {
            // Replace if already exists (idempotent re-confirm)
            if let Some(pos) = state.records.iter().position(|r| r.name == record.name) {
                state.records[pos] = record.clone();
            } else {
                state.records.push(record.clone());
            }
        }

        ProposedChange::ModifyBuffer {
            record_name,
            buffer,
            capacity,
        } => {
            let rec = state
                .records
                .iter_mut()
                .find(|r| &r.name == record_name)
                .ok_or_else(|| anyhow::anyhow!("record '{}' not found in state", record_name))?;
            rec.buffer = buffer.clone();
            rec.capacity = *capacity;
        }

        ProposedChange::AddConnector {
            record_name,
            connector,
        } => {
            let rec = state
                .records
                .iter_mut()
                .find(|r| &r.name == record_name)
                .ok_or_else(|| anyhow::anyhow!("record '{}' not found in state", record_name))?;
            rec.connectors.push(connector.clone());
        }

        ProposedChange::ModifyFields {
            record_name,
            fields,
        } => {
            let rec = state
                .records
                .iter_mut()
                .find(|r| &r.name == record_name)
                .ok_or_else(|| anyhow::anyhow!("record '{}' not found in state", record_name))?;
            rec.fields = fields.clone();
        }

        ProposedChange::RemoveRecord { record_name } => {
            state.records.retain(|r| &r.name != record_name);
        }

        ProposedChange::RenameRecord { old_name, new_name } => {
            for rec in &mut state.records {
                if &rec.name == old_name {
                    rec.name = new_name.clone();
                }
            }
            // Update decision log references
            for d in &mut state.decisions {
                if &d.record == old_name {
                    d.record = new_name.clone();
                }
            }
        }

        ProposedChange::ModifyKeyVariants {
            record_name,
            key_variants,
            key_prefix,
        } => {
            let rec = state
                .records
                .iter_mut()
                .find(|r| &r.name == record_name)
                .ok_or_else(|| anyhow::anyhow!("record '{}' not found in state", record_name))?;
            rec.key_variants = key_variants.clone();
            if let Some(prefix) = key_prefix {
                rec.key_prefix = prefix.clone();
            }
        }
    }

    Ok(())
}

/// Ensure `.aimdb/state.toml` exists with an initialised `[meta]` block.
/// Returns the current state (existing or freshly created).
pub fn ensure_state_initialised(path: &Path) -> anyhow::Result<ArchitectureState> {
    if let Some(existing) = read_state(path)? {
        return Ok(existing);
    }
    let state = ArchitectureState {
        project: None,
        meta: aimdb_codegen::Meta {
            aimdb_version: "0.5.0".to_string(),
            created_at: Utc::now().to_rfc3339(),
            last_modified: Utc::now().to_rfc3339(),
        },
        records: Vec::new(),
        decisions: Vec::new(),
    };
    write_state(path, &state)?;
    Ok(state)
}
