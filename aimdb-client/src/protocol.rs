//! AimX Protocol Types
//!
//! Re-exports the protocol types from `aimdb-core` that the client surface
//! uses. The NDJSON helper functions and legacy `Request`/`Response`
//! extension traits were retired with the hand-rolled client — the engine
//! ([`crate::engine::AimxConnection`]) owns framing and correlation.

// Re-export protocol types from aimdb-core
pub use aimdb_core::remote::{
    ErrorObject, Event, HelloMessage, RecordMetadata, Request, Response, WelcomeMessage,
    PROTOCOL_VERSION,
};

/// Client identifier
pub const CLIENT_NAME: &str = "aimdb-cli";
