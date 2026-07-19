//! AimX Protocol Message Types
//!
//! Defines the method-level payloads for the remote access protocol: the
//! hello/welcome handshake bodies and the request/response/event shapes.
//! These ride the **AimX-v2** NDJSON envelope; the wire framing itself lives
//! in [`crate::session::aimx`] (feature `connector-session`). See
//! `docs/design/remote-access-via-connectors.md` for the architecture.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Version of the AimX wire protocol spoken by this crate.
pub const PROTOCOL_VERSION: &str = "3.0";

/// Major-version component of a `"MAJOR.MINOR"` protocol string, or `None` if it
/// is empty or not in that shape.
fn protocol_major(version: &str) -> Option<&str> {
    match version.split('.').next() {
        Some(major) if !major.is_empty() => Some(major),
        _ => None,
    }
}

/// Whether a peer advertising `their_version` is wire-compatible with this
/// crate's [`PROTOCOL_VERSION`].
///
/// Compatibility is by **major** version — a breaking wire change bumps the
/// major (2.x → 3.0), a compatible additive change bumps only the minor. A
/// missing or malformed version string is treated as **incompatible** (fail
/// closed): a peer that cannot state a version it speaks is exactly the legacy
/// client this gate exists to turn away.
pub fn version_compatible(their_version: &str) -> bool {
    match (
        protocol_major(their_version),
        protocol_major(PROTOCOL_VERSION),
    ) {
        (Some(theirs), Some(ours)) => theirs == ours,
        _ => false,
    }
}

/// Client hello message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMessage {
    /// Protocol version
    pub version: String,

    /// Client identification string
    pub client: String,

    /// Desired capabilities (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,

    /// Authentication token (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

/// Server welcome message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeMessage {
    /// Protocol version
    pub version: String,

    /// Server identification string
    pub server: String,

    /// Granted permissions
    pub permissions: Vec<String>,

    /// Records that allow writes (empty for read-only)
    pub writable_records: Vec<String>,

    /// Maximum subscriptions per connection (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_subscriptions: Option<usize>,

    /// Whether client is authenticated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
}

/// Request message from client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Unique request identifier
    pub id: u64,

    /// Method name
    pub method: String,

    /// Method parameters (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<JsonValue>,
}

/// Response message from server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    /// Success response
    Success {
        /// Request ID
        id: u64,
        /// Result value
        result: JsonValue,
    },
    /// Error response
    Error {
        /// Request ID
        id: u64,
        /// Error details
        error: ErrorObject,
    },
}

impl Response {
    /// Creates a success response
    pub fn success(id: u64, result: JsonValue) -> Self {
        Self::Success { id, result }
    }

    /// Creates an error response
    pub fn error(id: u64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            id,
            error: ErrorObject {
                code: code.into(),
                message: message.into(),
                details: None,
            },
        }
    }

    /// Creates an error response with details
    pub fn error_with_details(
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
        details: JsonValue,
    ) -> Self {
        Self::Error {
            id,
            error: ErrorObject {
                code: code.into(),
                message: message.into(),
                details: Some(details),
            },
        }
    }
}

/// Error object in response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObject {
    /// Error code
    pub code: String,

    /// Human-readable error message
    pub message: String,

    /// Additional error details (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// Event message from server (subscription push)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Subscription identifier
    pub subscription_id: String,

    /// Monotonic sequence number
    pub sequence: u64,

    /// Event data (record value)
    pub data: JsonValue,

    /// Unix timestamp in "secs.nanosecs" format (e.g., "1730379296.123456789")
    pub timestamp: String,

    /// Number of dropped events since last delivery (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn test_hello_serialization() {
        let hello = HelloMessage {
            version: PROTOCOL_VERSION.to_string(),
            client: "test-client".to_string(),
            capabilities: Some(vec!["read".to_string()]),
            auth_token: None,
        };

        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("\"version\":\"3.0\""));
        assert!(json.contains("\"client\":\"test-client\""));
    }

    #[test]
    fn version_compatible_matches_on_major() {
        // Same major (current + a hypothetical future minor) → compatible.
        assert!(version_compatible(PROTOCOL_VERSION));
        assert!(version_compatible("3.0"));
        assert!(version_compatible("3.7"));
        assert!(version_compatible("3")); // bare major

        // Older major (the legacy WS-era wire) → refused.
        assert!(!version_compatible("2.0"));
        assert!(!version_compatible("1.0"));

        // Missing / malformed → fail closed.
        assert!(!version_compatible(""));
        assert!(!version_compatible(".0"));
        assert!(!version_compatible("garbage"));
    }

    #[test]
    fn test_request_serialization() {
        let request = Request {
            id: 1,
            method: "record.list".to_string(),
            params: Some(serde_json::json!({})),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"record.list\""));
    }

    #[test]
    fn test_response_success() {
        let response = Response::success(1, serde_json::json!({"status": "ok"}));

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"status\":\"ok\""));
    }

    #[test]
    fn test_response_error() {
        let response = Response::error(2, "NOT_FOUND", "Record not found");

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"id\":2"));
        assert!(json.contains("\"error\""));
        assert!(json.contains("\"code\":\"NOT_FOUND\""));
        assert!(json.contains("\"message\":\"Record not found\""));
    }

    #[test]
    fn test_event_serialization() {
        let event = Event {
            subscription_id: "sub-123".to_string(),
            sequence: 42,
            data: serde_json::json!({"temp": 23.5}),
            timestamp: "1730379296.123456789".to_string(),
            dropped: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"subscription_id\":\"sub-123\""));
        assert!(json.contains("\"sequence\":42"));
        assert!(json.contains("\"temp\":23.5"));
    }

    #[test]
    fn test_event_with_dropped() {
        let event = Event {
            subscription_id: "sub-456".to_string(),
            sequence: 100,
            data: serde_json::json!({"value": 1}),
            timestamp: "1730379300.987654321".to_string(),
            dropped: Some(5),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"dropped\":5"));
    }
}
