//! Wire protocol types and topic matching for the WebSocket connector.
//!
//! This module re-exports all types from [`aimdb_ws_protocol`] for backwards
//! compatibility. The canonical definitions live in the shared protocol crate
//! so they can be used by both the server connector and browser/native clients.

// Re-export everything from the shared protocol crate
pub use aimdb_ws_protocol::{
    now_ms, topic_matches, ClientMessage, ErrorCode, QueryRecord, ServerMessage,
};
