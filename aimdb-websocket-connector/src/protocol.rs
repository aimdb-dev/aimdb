//! Wire protocol types and topic matching for the WebSocket connector.
//!
//! This module re-exports all types from [`aimdb_ws_protocol`] for backwards
//! compatibility. The canonical definitions now live in the shared `no_std`
//! protocol crate so they can be used by both the server connector and
//! browser/native clients.
//!
//! # Server → Client messages
//!
//! - `data` — live record push
//! - `snapshot` — late-join current value
//! - `subscribed` — subscription acknowledgement
//! - `error` — per-operation error
//! - `pong` — response to client ping
//!
//! # Client → Server messages
//!
//! - `subscribe` — subscribe to one or more topics (supports wildcards)
//! - `unsubscribe` — cancel subscriptions
//! - `write` — inbound value for a `link_from("ws://…")` record
//! - `ping` — keepalive ping

// Re-export everything from the shared protocol crate
pub use aimdb_ws_protocol::{topic_matches, ClientMessage, ErrorCode, ServerMessage};

/// Returns the current milliseconds since the Unix epoch (for `ts` fields).
pub fn now_ms() -> u64 {
    aimdb_ws_protocol::now_ms()
}
