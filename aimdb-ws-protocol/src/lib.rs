//! # aimdb-ws-protocol
//!
//! Shared wire protocol types for the AimDB WebSocket connector ecosystem.
//!
//! This crate is `no_std + alloc` compatible so it can be used from:
//!
//! - **`aimdb-websocket-connector`** — the server side (Axum/Tokio)
//! - **`aimdb-wasm-adapter`** — the browser client (`WsBridge`)
//! - **Future native WS client** — Tokio/Embassy client connector
//!
//! # Wire Protocol
//!
//! All messages are JSON-encoded with a `"type"` discriminant tag:
//!
//! ## Server → Client ([`ServerMessage`])
//!
//! - `data` — live record push with timestamp
//! - `snapshot` — late-join current value
//! - `subscribed` — subscription acknowledgement
//! - `error` — per-operation error
//! - `pong` — response to client ping
//!
//! ## Client → Server ([`ClientMessage`])
//!
//! - `subscribe` — subscribe to one or more topics (supports MQTT wildcards)
//! - `unsubscribe` — cancel subscriptions
//! - `write` — inbound value for a `link_from("ws://…")` record
//! - `ping` — keepalive ping
//!
//! # Topic Matching
//!
//! [`topic_matches`] implements MQTT-style wildcard matching (`#` for
//! multi-level, `*` for single-level).

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════════════
// Server → Client
// ════════════════════════════════════════════════════════════════════

/// A message sent from the server to a connected WebSocket client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Live data push from an outbound route.
    Data {
        topic: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        /// Server-side dispatch timestamp (milliseconds since Unix epoch).
        ts: u64,
    },

    /// Late-join snapshot — current value sent when a client subscribes.
    Snapshot {
        topic: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
    },

    /// Confirmation sent once subscriptions are recorded.
    Subscribed { topics: Vec<String> },

    /// Per-operation error.
    Error {
        code: ErrorCode,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
        message: String,
    },

    /// Response to a client `ping` message.
    Pong,
}

/// Machine-readable error codes sent in `ServerMessage::Error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    UnknownTopic,
    SerializationError,
    WriteError,
    ServerError,
}

// ════════════════════════════════════════════════════════════════════
// Client → Server
// ════════════════════════════════════════════════════════════════════

/// A message received from a WebSocket client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Subscribe to one or more topics (wildcards supported).
    Subscribe { topics: Vec<String> },

    /// Unsubscribe from one or more topics.
    Unsubscribe { topics: Vec<String> },

    /// Write a value to an inbound record (`link_from("ws://…")`).
    Write {
        topic: String,
        payload: serde_json::Value,
    },

    /// Keepalive ping.
    Ping,
}

// ════════════════════════════════════════════════════════════════════
// Topic matching
// ════════════════════════════════════════════════════════════════════

/// Returns `true` if `topic` matches `pattern`.
///
/// Follows MQTT wildcard conventions:
///
/// | Pattern  | Semantics                         |
/// |----------|-----------------------------------|
/// | `#`      | Multi-level wildcard (all topics) |
/// | `a/#`    | Everything under `a/`             |
/// | `a/*/c`  | Single-level wildcard in segment  |
/// | `a/b/c`  | Exact match                       |
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    // Fast path: exact match
    if pattern == topic {
        return true;
    }

    // Multi-level wildcard: `#` matches everything
    if pattern == "#" {
        return true;
    }

    // `prefix/#` matches everything under prefix — only when prefix is literal
    // (no wildcards in the prefix). When wildcards are present, fall through to
    // the segment loop which handles `#` at any position.
    if let Some(prefix) = pattern.strip_suffix("/#") {
        if !prefix.contains('*') && !prefix.contains('#') {
            return topic.starts_with(prefix)
                && (topic.len() == prefix.len()
                    || topic.as_bytes().get(prefix.len()) == Some(&b'/'));
        }
    }

    // Segment-by-segment matching with `*` single-level wildcard
    let mut pattern_parts = pattern.split('/');
    let mut topic_parts = topic.split('/');

    loop {
        match (pattern_parts.next(), topic_parts.next()) {
            (Some("#"), _) => return true,
            (Some("*"), Some(_)) => {} // single-level wildcard — consume one segment
            (Some(p), Some(t)) if p == t => {} // literal match
            (None, None) => return true, // both exhausted at the same time
            _ => return false,
        }
    }
}

/// Returns the current milliseconds since the Unix epoch (for `ts` fields).
///
/// Requires the `std` feature.
#[cfg(feature = "std")]
pub fn now_ms() -> u64 {
    extern crate std;
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn exact_match() {
        assert!(topic_matches("a/b/c", "a/b/c"));
        assert!(!topic_matches("a/b/c", "a/b/d"));
    }

    #[test]
    fn hash_wildcard() {
        assert!(topic_matches("#", "anything/goes/here"));
        assert!(topic_matches("#", "a"));
    }

    #[test]
    fn prefix_hash_wildcard() {
        assert!(topic_matches("sensors/#", "sensors/temperature/vienna"));
        assert!(topic_matches("sensors/#", "sensors/humidity/berlin"));
        assert!(!topic_matches("sensors/#", "commands/setpoint"));
        // Edge: prefix itself
        assert!(topic_matches("sensors/#", "sensors"));
    }

    #[test]
    fn star_wildcard() {
        assert!(topic_matches(
            "sensors/temperature/*",
            "sensors/temperature/vienna"
        ));
        assert!(topic_matches(
            "sensors/temperature/*",
            "sensors/temperature/berlin"
        ));
        assert!(!topic_matches(
            "sensors/temperature/*",
            "sensors/humidity/vienna"
        ));
        assert!(!topic_matches(
            "sensors/temperature/*",
            "sensors/temperature/a/b"
        ));
    }

    #[test]
    fn mixed_wildcards() {
        assert!(topic_matches("a/*/c/#", "a/b/c/d/e/f"));
        assert!(!topic_matches("a/*/c/#", "a/b/x/d"));
    }

    #[test]
    fn serde_server_message_roundtrip() {
        let msg = ServerMessage::Data {
            topic: "sensors/temp".into(),
            payload: Some(serde_json::json!({"celsius": 21.5})),
            ts: 1234567890,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::Data { topic, ts, .. } => {
                assert_eq!(topic, "sensors/temp");
                assert_eq!(ts, 1234567890);
            }
            _ => panic!("Expected Data variant"),
        }
    }

    #[test]
    fn serde_client_message_roundtrip() {
        let msg = ClientMessage::Subscribe {
            topics: alloc::vec!["sensors/#".into()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Subscribe { topics } => {
                assert_eq!(topics, alloc::vec!["sensors/#".to_string()]);
            }
            _ => panic!("Expected Subscribe variant"),
        }
    }

    #[test]
    fn serde_error_code_roundtrip() {
        let msg = ServerMessage::Error {
            code: ErrorCode::UnknownTopic,
            topic: Some("foo/bar".into()),
            message: "not found".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("UNKNOWN_TOPIC"));
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::Error { code, .. } => {
                assert!(matches!(code, ErrorCode::UnknownTopic));
            }
            _ => panic!("Expected Error variant"),
        }
    }
}
