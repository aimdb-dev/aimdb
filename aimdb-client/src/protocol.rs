//! AimX Protocol Types
//!
//! Re-exports the protocol types from `aimdb-core` that the client surface
//! uses. Framing and correlation live in the engine
//! ([`crate::engine::AimxConnection`]), not here.

// Re-export protocol types from aimdb-core
pub use aimdb_core::remote::{
    version_compatible, BuildReply, ErrorObject, Event, HelloMessage, InstanceIdentityReply,
    InstanceInfoReply, PostLinkChunk, PostLinkReply, RecordMetadata, Request, Response,
    WelcomeMessage, POST_LINK_CHUNK_MAX, PROTOCOL_VERSION,
};

/// Client identifier
pub const CLIENT_NAME: &str = "aimdb-cli";
