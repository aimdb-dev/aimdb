//! Session-scoped state machine for the architecture ideation loop.
//!
//! Enforces the transition sequence:
//! `Idle → Gathering → Proposing → (resolve) → Gathering → ...`
//!
//! Only one proposal may be pending at a time. Read-only tools
//! (`get_architecture`, `get_buffer_metrics`, `validate_against_instance`)
//! are allowed in any phase.

use super::Proposal;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

// ── Monotonic ID generator (fixes millisecond collision) ─────────────────────

static PROPOSAL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique proposal ID. Uses a monotonic counter combined with
/// a timestamp prefix for human readability. Never collides.
pub fn next_proposal_id() -> String {
    let seq = PROPOSAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("prop-{}-{:04}", Utc::now().format("%Y%m%d%H%M%S"), seq)
}

// ── Session phases ───────────────────────────────────────────────────────────

/// The phases of the ideation loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// No active work. Waiting for `get_architecture` to start gathering.
    Idle,
    /// LLM is reading state and asking disambiguation questions.
    Gathering,
    /// A single proposal is pending human review.
    Proposing,
    /// A confirmed proposal is being applied to state.toml and artefacts.
    Applying,
}

impl SessionPhase {
    /// Human-readable hint for the LLM about what to do next in this phase.
    pub fn guidance(&self) -> &'static str {
        match self {
            SessionPhase::Idle => {
                "Call get_architecture to read the current state and begin a new ideation cycle."
            }
            SessionPhase::Gathering => {
                "Ask the user disambiguation questions, then call the appropriate propose_* tool \
                 (propose_add_record, propose_modify_buffer, etc.) when you have enough context \
                 to make a concrete proposal."
            }
            SessionPhase::Proposing => {
                "A proposal is pending. Present it to the user and call \
                 resolve_proposal with their decision (confirm/reject/revise)."
            }
            SessionPhase::Applying => {
                "A proposal is being applied. Wait for the operation to complete."
            }
        }
    }
}

// ── Gathering context ────────────────────────────────────────────────────────

/// Tracks what has been resolved during the Gathering phase.
///
/// This is informational — it helps error messages be specific about
/// what is still unresolved, but does NOT block the LLM from proposing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatheringContext {
    /// The target record name (if identified).
    pub target_record: Option<String>,
    /// Whether buffer type has been disambiguated.
    pub buffer_resolved: bool,
    /// Whether fields/value struct has been discussed.
    pub fields_resolved: bool,
    /// Whether key variants have been enumerated.
    pub variants_resolved: bool,
    /// Free-form notes the LLM can attach during gathering.
    pub notes: Vec<String>,
}

// ── Session ──────────────────────────────────────────────────────────────────

/// A single ideation session. One session is active at a time.
/// The session owns at most ONE pending proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Current phase of the ideation loop.
    pub phase: SessionPhase,
    /// When this session was created.
    pub created_at: String,
    /// Context accumulated during the Gathering phase.
    pub gathering: GatheringContext,
    /// The single pending proposal (only set in Proposing phase).
    pub pending_proposal: Option<Proposal>,
    /// Count of proposals resolved in this session (for statistics).
    pub resolved_count: u32,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// Create a new session in the Idle phase.
    pub fn new() -> Self {
        let id = format!("session-{}", Utc::now().format("%Y%m%d%H%M%S%3f"));
        Self {
            id,
            phase: SessionPhase::Idle,
            created_at: Utc::now().to_rfc3339(),
            gathering: GatheringContext::default(),
            pending_proposal: None,
            resolved_count: 0,
        }
    }
}

// ── Session error ────────────────────────────────────────────────────────────

/// Error produced by invalid state transitions. Includes phase-aware
/// guidance so the LLM knows exactly what to do next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionError {
    /// Current phase when the invalid operation was attempted.
    pub current_phase: SessionPhase,
    /// What the caller tried to do.
    pub attempted_action: String,
    /// Human-readable guidance for the LLM.
    pub guidance: String,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Invalid in phase {:?}: {}. Next step: {}",
            self.current_phase, self.attempted_action, self.guidance
        )
    }
}

// ── Gathering update ─────────────────────────────────────────────────────────

/// Partial update to gathering context.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GatheringUpdate {
    pub target_record: Option<String>,
    pub buffer_resolved: Option<bool>,
    pub fields_resolved: Option<bool>,
    pub variants_resolved: Option<bool>,
    pub note: Option<String>,
}

// ── Session store ────────────────────────────────────────────────────────────

/// Replaces the old `ProposalStore`. Manages a single active session
/// with at most one pending proposal.
#[derive(Debug, Default)]
pub struct SessionStore {
    /// The active session, if any. `None` means no session has started.
    pub active: Option<Session>,
}

impl SessionStore {
    // ── Transition: Idle → Gathering (triggered by get_architecture) ─────

    /// Called when `get_architecture` is invoked. Creates a session if
    /// none exists, and transitions Idle → Gathering.
    ///
    /// If already in Gathering/Proposing, this is a no-op read (allowed
    /// at any time — reading state is always safe).
    pub fn on_get_architecture(&mut self) -> &Session {
        let session = self.active.get_or_insert_with(Session::new);
        if session.phase == SessionPhase::Idle {
            session.phase = SessionPhase::Gathering;
        }
        session
    }

    // ── Transition: Gathering → Proposing (triggered by propose_* tools) ──

    /// Called when `propose_add_record`, `propose_modify_*`, `remove_record`,
    /// or `rename_record` is invoked. Validates the transition and stores the single proposal.
    pub fn on_propose(&mut self, proposal: Proposal) -> Result<&Proposal, SessionError> {
        let session = self.active.as_mut().ok_or_else(|| SessionError {
            current_phase: SessionPhase::Idle,
            attempted_action: "propose_* (no active session — call get_architecture first)"
                .to_string(),
            guidance: SessionPhase::Idle.guidance().to_string(),
        })?;

        match session.phase {
            SessionPhase::Idle => Err(SessionError {
                current_phase: SessionPhase::Idle,
                attempted_action: "propose_* (session is Idle — call get_architecture first)"
                    .to_string(),
                guidance: SessionPhase::Idle.guidance().to_string(),
            }),
            SessionPhase::Gathering => {
                session.phase = SessionPhase::Proposing;
                session.pending_proposal = Some(proposal);
                Ok(session.pending_proposal.as_ref().unwrap())
            }
            SessionPhase::Proposing => Err(SessionError {
                current_phase: SessionPhase::Proposing,
                attempted_action:
                    "propose_* (a proposal is already pending — resolve or reset first)".to_string(),
                guidance: SessionPhase::Proposing.guidance().to_string(),
            }),
            SessionPhase::Applying => Err(SessionError {
                current_phase: SessionPhase::Applying,
                attempted_action: "propose_* (cannot propose while applying — wait or reset)"
                    .to_string(),
                guidance: SessionPhase::Applying.guidance().to_string(),
            }),
        }
    }

    // ── Transition: Proposing → Gathering (triggered by resolve_proposal)

    /// Called when `resolve_proposal` is invoked. Takes the pending proposal
    /// out of the session and transitions back to Gathering.
    pub fn on_resolve(&mut self, proposal_id: &str) -> Result<Proposal, SessionError> {
        let session = self.active.as_mut().ok_or_else(|| SessionError {
            current_phase: SessionPhase::Idle,
            attempted_action: "resolve_proposal".to_string(),
            guidance: SessionPhase::Idle.guidance().to_string(),
        })?;

        if session.phase != SessionPhase::Proposing {
            return Err(SessionError {
                current_phase: session.phase,
                attempted_action: "resolve_proposal".to_string(),
                guidance: session.phase.guidance().to_string(),
            });
        }

        let proposal = session
            .pending_proposal
            .take()
            .ok_or_else(|| SessionError {
                current_phase: SessionPhase::Proposing,
                attempted_action: "resolve_proposal (no proposal found)".to_string(),
                guidance: "Internal error: phase is Proposing but no proposal stored.".to_string(),
            })?;

        if proposal.id != proposal_id {
            // Put it back — wrong ID
            let correct_id = proposal.id.clone();
            session.pending_proposal = Some(proposal);
            return Err(SessionError {
                current_phase: SessionPhase::Proposing,
                attempted_action: format!("resolve_proposal with id '{proposal_id}'"),
                guidance: format!(
                    "The pending proposal has id '{}'. Use that id instead.",
                    correct_id
                ),
            });
        }

        session.resolved_count += 1;
        session.phase = SessionPhase::Gathering;
        session.gathering = GatheringContext::default();

        Ok(proposal)
    }

    // ── Update gathering context ─────────────────────────────────────────

    /// Let the LLM annotate what it has resolved during gathering.
    pub fn update_gathering(&mut self, update: GatheringUpdate) -> Result<(), SessionError> {
        let session = self.active.as_mut().ok_or_else(|| SessionError {
            current_phase: SessionPhase::Idle,
            attempted_action: "update_gathering".to_string(),
            guidance: SessionPhase::Idle.guidance().to_string(),
        })?;

        if session.phase != SessionPhase::Gathering {
            return Err(SessionError {
                current_phase: session.phase,
                attempted_action: "update_gathering".to_string(),
                guidance: session.phase.guidance().to_string(),
            });
        }

        if let Some(name) = update.target_record {
            session.gathering.target_record = Some(name);
        }
        if let Some(v) = update.buffer_resolved {
            session.gathering.buffer_resolved = v;
        }
        if let Some(v) = update.fields_resolved {
            session.gathering.fields_resolved = v;
        }
        if let Some(v) = update.variants_resolved {
            session.gathering.variants_resolved = v;
        }
        if let Some(note) = update.note {
            session.gathering.notes.push(note);
        }
        Ok(())
    }

    /// Reset the session entirely (user wants to start over).
    pub fn reset(&mut self) {
        self.active = None;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::ProposedChange;
    use aimdb_codegen::{BufferType, FieldDef, RecordDef};

    fn test_proposal(id: &str) -> Proposal {
        Proposal {
            id: id.to_string(),
            change_type: "add_record".to_string(),
            description: "Test proposal".to_string(),
            change: ProposedChange::AddRecord {
                record: RecordDef {
                    name: "TestRecord".to_string(),
                    buffer: BufferType::SingleLatest,
                    capacity: None,
                    key_prefix: "test.".to_string(),
                    key_variants: vec!["alpha".to_string()],
                    producers: vec!["producer".to_string()],
                    consumers: vec!["consumer".to_string()],
                    schema_version: None,
                    serialization: None,
                    observable: None,
                    fields: vec![FieldDef {
                        name: "value".to_string(),
                        field_type: "f64".to_string(),
                        description: "Test value".to_string(),
                        settable: false,
                    }],
                    connectors: vec![],
                },
            },
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_idle_to_gathering() {
        let mut store = SessionStore::default();
        assert!(store.active.is_none());

        let session = store.on_get_architecture();
        assert_eq!(session.phase, SessionPhase::Gathering);
        assert!(store.active.is_some());
    }

    #[test]
    fn test_get_architecture_idempotent_in_gathering() {
        let mut store = SessionStore::default();
        let id1 = store.on_get_architecture().id.clone();
        let id2 = store.on_get_architecture().id.clone();
        assert_eq!(id1, id2, "Should reuse the same session");
        assert_eq!(
            store.active.as_ref().unwrap().phase,
            SessionPhase::Gathering
        );
    }

    #[test]
    fn test_gathering_to_proposing() {
        let mut store = SessionStore::default();
        store.on_get_architecture();

        let proposal = test_proposal("prop-001");
        let result = store.on_propose(proposal);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "prop-001");
        assert_eq!(
            store.active.as_ref().unwrap().phase,
            SessionPhase::Proposing
        );
    }

    #[test]
    fn test_double_propose_rejected() {
        let mut store = SessionStore::default();
        store.on_get_architecture();

        store.on_propose(test_proposal("prop-001")).unwrap();

        let result = store.on_propose(test_proposal("prop-002"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.current_phase, SessionPhase::Proposing);
    }

    #[test]
    fn test_propose_without_session_rejected() {
        let mut store = SessionStore::default();
        let result = store.on_propose(test_proposal("prop-001"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.current_phase, SessionPhase::Idle);
    }

    #[test]
    fn test_propose_in_idle_rejected() {
        let mut store = SessionStore {
            active: Some(Session::new()),
        };
        assert_eq!(store.active.as_ref().unwrap().phase, SessionPhase::Idle);

        let result = store.on_propose(test_proposal("prop-001"));
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_returns_to_gathering() {
        let mut store = SessionStore::default();
        store.on_get_architecture();
        store.on_propose(test_proposal("prop-001")).unwrap();

        let proposal = store.on_resolve("prop-001").unwrap();
        assert_eq!(proposal.id, "prop-001");
        assert_eq!(
            store.active.as_ref().unwrap().phase,
            SessionPhase::Gathering
        );
        assert_eq!(store.active.as_ref().unwrap().resolved_count, 1);
    }

    #[test]
    fn test_resolve_wrong_id_rejected() {
        let mut store = SessionStore::default();
        store.on_get_architecture();
        store.on_propose(test_proposal("prop-001")).unwrap();

        let result = store.on_resolve("prop-999");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.guidance.contains("prop-001"));

        // Proposal should still be pending
        assert_eq!(
            store.active.as_ref().unwrap().phase,
            SessionPhase::Proposing
        );
        assert!(store.active.as_ref().unwrap().pending_proposal.is_some());
    }

    #[test]
    fn test_resolve_not_in_proposing_rejected() {
        let mut store = SessionStore::default();
        store.on_get_architecture();

        let result = store.on_resolve("prop-001");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().current_phase, SessionPhase::Gathering);
    }

    #[test]
    fn test_reset_clears_session() {
        let mut store = SessionStore::default();
        store.on_get_architecture();
        store.on_propose(test_proposal("prop-001")).unwrap();

        store.reset();
        assert!(store.active.is_none());
    }

    #[test]
    fn test_proposal_id_uniqueness() {
        let ids: Vec<String> = (0..100).map(|_| next_proposal_id()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "All IDs should be unique");
    }

    #[test]
    fn test_full_cycle() {
        let mut store = SessionStore::default();

        // Cycle 1: get_architecture → propose → resolve(confirm)
        store.on_get_architecture();
        store.on_propose(test_proposal("prop-001")).unwrap();
        store.on_resolve("prop-001").unwrap();

        // Should be back in Gathering for the next record
        assert_eq!(
            store.active.as_ref().unwrap().phase,
            SessionPhase::Gathering
        );

        // Cycle 2: propose again
        store.on_propose(test_proposal("prop-002")).unwrap();
        store.on_resolve("prop-002").unwrap();

        assert_eq!(store.active.as_ref().unwrap().resolved_count, 2);
    }

    #[test]
    fn test_update_gathering() {
        let mut store = SessionStore::default();
        store.on_get_architecture();

        store
            .update_gathering(GatheringUpdate {
                target_record: Some("Temperature".to_string()),
                buffer_resolved: Some(true),
                ..Default::default()
            })
            .unwrap();

        let ctx = &store.active.as_ref().unwrap().gathering;
        assert_eq!(ctx.target_record.as_deref(), Some("Temperature"));
        assert!(ctx.buffer_resolved);
        assert!(!ctx.fields_resolved);
    }

    #[test]
    fn test_update_gathering_not_in_gathering_rejected() {
        let mut store = SessionStore::default();
        store.on_get_architecture();
        store.on_propose(test_proposal("prop-001")).unwrap();

        let result = store.update_gathering(GatheringUpdate::default());
        assert!(result.is_err());
    }
}
