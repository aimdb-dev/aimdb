//! AimDB Codegen — architecture state to Mermaid and Rust source
//!
//! This library reads `.aimdb/state.toml` (the architecture agent's decision
//! record) and emits two artefacts:
//!
//! - **Mermaid diagram** — `.aimdb/architecture.mermaid`, a read-only graph
//!   projection of the architecture (see [`generate_mermaid`])
//! - **Rust source** — `src/generated_schema.rs`, compilable AimDB schema
//!   using the actual 0.5.x API (see [`generate_rust`])
//!
//! # Usage
//!
//! ```rust
//! use aimdb_codegen::{ArchitectureState, generate_mermaid, generate_rust, validate};
//!
//! let toml = r#"
//! [meta]
//! aimdb_version = "0.5.0"
//! created_at = "2026-02-22T14:00:00Z"
//! last_modified = "2026-02-22T14:00:00Z"
//!
//! [[records]]
//! name = "Temperature"
//! buffer = "SpmcRing"
//! capacity = 128
//! key_prefix = "sensor."
//! key_variants = ["room1"]
//! producers = ["sensor_task"]
//! consumers = ["dashboard"]
//!
//! [[records.fields]]
//! name = "celsius"
//! type = "f64"
//! description = "Temperature in Celsius"
//! "#;
//!
//! let state = ArchitectureState::from_toml(toml).unwrap();
//!
//! let errors = validate(&state);
//! assert!(errors.iter().all(|e| e.severity != validate::Severity::Error));
//!
//! let mermaid = generate_mermaid(&state);
//! assert!(mermaid.contains("flowchart LR"));
//!
//! let rust = generate_rust(&state);
//! assert!(rust.contains("pub struct TemperatureValue"));
//! ```

pub mod mermaid;
pub mod rust;
pub mod state;
pub mod validate;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use mermaid::generate_mermaid;
pub use rust::{generate_cargo_toml, generate_lib_rs, generate_rust, generate_schema_rs};
pub use rust::{to_pascal_case, to_snake_case};
pub use state::{
    ArchitectureState, BufferType, ConnectorDef, ConnectorDirection, DecisionEntry, FieldDef, Meta,
    ObservableDef, ProjectDef, RecordDef, SerializationType,
};
pub use validate::{is_valid, validate, Severity, ValidationError};
