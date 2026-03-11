//! # aimdb-ws-protocol
//!
//! Shared wire protocol types for the AimDB WebSocket connector ecosystem.
//!
//! Used by:
//!
//! - **`aimdb-websocket-connector`** — the server side (Axum/Tokio)
//! - **`aimdb-wasm-adapter`** — the browser client (`WsBridge`)
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
//! - `query_result` — response to a client query request
//!
//! ## Client → Server ([`ClientMessage`])
//!
//! - `subscribe` — subscribe to one or more topics (supports MQTT wildcards)
//! - `unsubscribe` — cancel subscriptions
//! - `write` — inbound value for a `link_from("ws://…")` record
//! - `ping` — keepalive ping
//! - `query` — query historical / persisted records
//!
//! # Topic Matching
//!
//! [`topic_matches`] implements MQTT-style wildcard matching (`#` for
//! multi-level, `*` for single-level).

use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════════════
// Server → Client
// ════════════════════════════════════════════════════════════════════

/// A message sent from the server to a connected WebSocket client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Response to a client `query` request.
    ///
    /// Contains the matching historical records and metadata.
    QueryResult {
        /// Correlation ID echoed from the client request.
        id: String,
        /// Matching records, ordered by timestamp ascending.
        records: Vec<QueryRecord>,
        /// Total number of records matched (before any limit).
        total: usize,
    },

    /// Response to a client `list_topics` request.
    TopicList {
        /// Correlation ID echoed from the client request.
        id: String,
        /// All outbound topics served by this endpoint.
        topics: Vec<TopicInfo>,
    },
}

/// A single record returned in a [`ServerMessage::QueryResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRecord {
    /// Topic / record name (e.g. `"temp.vienna"`).
    pub topic: String,
    /// Deserialized record value.
    pub payload: serde_json::Value,
    /// Storage timestamp (milliseconds since Unix epoch).
    pub ts: u64,
}

/// Metadata for a single outbound topic served by a WebSocket endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicInfo {
    /// Record key / topic name (e.g. `"temp.vienna"`).
    pub name: String,
    /// Schema type name (e.g. `"temperature"`), if known by the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    /// Entity / node identifier (e.g. `"vienna"`), extracted server-side from the
    /// topic name. The server is the authority on naming conventions — clients
    /// should use this field directly rather than parsing the topic name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

/// Machine-readable error codes sent in `ServerMessage::Error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Query historical / persisted records.
    ///
    /// The server responds with [`ServerMessage::QueryResult`] carrying the
    /// same `id` for correlation.
    Query {
        /// Client-generated correlation ID (echoed in the response).
        id: String,
        /// Topic pattern to match (MQTT wildcards supported, `"*"` for all).
        pattern: String,
        /// Start of time range (milliseconds since Unix epoch), inclusive. Defaults to 1 h ago.
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<u64>,
        /// End of time range (milliseconds since Unix epoch), inclusive. Defaults to now.
        #[serde(skip_serializing_if = "Option::is_none")]
        to: Option<u64>,
        /// Maximum number of records to return per matching topic.
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },

    /// Request the list of topics served by this WebSocket endpoint.
    ///
    /// The server responds with [`ServerMessage::TopicList`] carrying the
    /// same `id` for correlation.
    ListTopics {
        /// Client-generated correlation ID (echoed in the response).
        id: String,
    },
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
pub fn now_ms() -> u64 {
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
            topics: vec!["sensors/#".into()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Subscribe { topics } => {
                assert_eq!(topics, vec!["sensors/#".to_string()]);
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
