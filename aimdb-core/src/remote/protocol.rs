//! AimX v1 Protocol Message Types
//!
//! Defines request, response, and event types for the remote access protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::{string::String, vec::Vec};

// Allow dead code for now - these are part of the public API for future implementation
#[allow(dead_code)]
pub const PROTOCOL_VERSION: &str = "1.0";

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

    /// ISO 8601 timestamp
    pub timestamp: String,

    /// Number of dropped events since last delivery (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

/// Top-level message envelope for protocol communication
#[allow(dead_code)] // Part of public API for future use
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// Client hello
    Hello { hello: HelloMessage },
    /// Server welcome
    Welcome { welcome: WelcomeMessage },
    /// Client request
    Request(Request),
    /// Server response
    Response(Response),
    /// Server event
    Event { event: Event },
}

#[allow(dead_code)] // Helper methods for future implementation
impl Message {
    /// Creates a hello message
    pub fn hello(client: impl Into<String>) -> Self {
        Self::Hello {
            hello: HelloMessage {
                version: PROTOCOL_VERSION.to_string(),
                client: client.into(),
                capabilities: None,
                auth_token: None,
            },
        }
    }

    /// Creates a welcome message
    pub fn welcome(server: impl Into<String>, permissions: Vec<String>) -> Self {
        Self::Welcome {
            welcome: WelcomeMessage {
                version: PROTOCOL_VERSION.to_string(),
                server: server.into(),
                permissions,
                writable_records: Vec::new(),
                max_subscriptions: None,
                authenticated: None,
            },
        }
    }

    /// Creates a request message
    pub fn request(id: u64, method: impl Into<String>, params: Option<JsonValue>) -> Self {
        Self::Request(Request {
            id,
            method: method.into(),
            params,
        })
    }

    /// Creates a success response message
    pub fn response_success(id: u64, result: JsonValue) -> Self {
        Self::Response(Response::success(id, result))
    }

    /// Creates an error response message
    pub fn response_error(id: u64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Response(Response::error(id, code, message))
    }

    /// Creates an event message
    pub fn event(
        subscription_id: impl Into<String>,
        sequence: u64,
        data: JsonValue,
        timestamp: impl Into<String>,
    ) -> Self {
        Self::Event {
            event: Event {
                subscription_id: subscription_id.into(),
                sequence,
                data,
                timestamp: timestamp.into(),
                dropped: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello_serialization() {
        let hello = HelloMessage {
            version: "1.0".to_string(),
            client: "test-client".to_string(),
            capabilities: Some(vec!["read".to_string()]),
            auth_token: None,
        };

        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("\"version\":\"1.0\""));
        assert!(json.contains("\"client\":\"test-client\""));
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
            timestamp: "2025-10-31T12:34:56.789Z".to_string(),
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
            timestamp: "2025-10-31T12:35:00.000Z".to_string(),
            dropped: Some(5),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"dropped\":5"));
    }
}
