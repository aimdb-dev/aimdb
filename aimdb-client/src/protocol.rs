//! AimX Protocol Types and Utilities
//!
//! This module re-exports protocol types from `aimdb-core` and adds CLI-specific
//! utilities for NDJSON serialization and convenience methods.

use serde::{Deserialize, Serialize};

// Re-export protocol types from aimdb-core
pub use aimdb_core::remote::{
    ErrorObject, Event, HelloMessage, RecordMetadata, Request, Response, WelcomeMessage,
};

/// Protocol version supported by this client
pub const PROTOCOL_VERSION: &str = "1.0";

/// Client identifier
pub const CLIENT_NAME: &str = "aimdb-cli";

/// CLI-specific convenience methods for Request
pub trait RequestExt {
    /// Create a new request
    #[allow(clippy::new_ret_no_self)]
    fn new(id: u64, method: impl Into<String>) -> Request;

    /// Create a request with parameters
    fn with_params(id: u64, method: impl Into<String>, params: serde_json::Value) -> Request;
}

impl RequestExt for Request {
    fn new(id: u64, method: impl Into<String>) -> Request {
        Request {
            id,
            method: method.into(),
            params: None,
        }
    }

    fn with_params(id: u64, method: impl Into<String>, params: serde_json::Value) -> Request {
        Request {
            id,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// CLI-specific convenience methods for Response
pub trait ResponseExt {
    /// Extract result or return error
    fn into_result(self) -> Result<serde_json::Value, ErrorObject>;
}

impl ResponseExt for Response {
    fn into_result(self) -> Result<serde_json::Value, ErrorObject> {
        match self {
            Response::Success { result, .. } => Ok(result),
            Response::Error { error, .. } => Err(error),
        }
    }
}

/// Create default CLI hello message
pub fn cli_hello() -> HelloMessage {
    HelloMessage {
        version: PROTOCOL_VERSION.to_string(),
        client: CLIENT_NAME.to_string(),
        capabilities: None,
        auth_token: None,
    }
}

/// Event message wrapper from subscription (for NDJSON parsing)
#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    pub event: Event,
}

/// Helper to serialize a message as NDJSON line
pub fn serialize_message<T: Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string(msg)?))
}

/// Helper to parse NDJSON line into message
pub fn parse_message<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = Request::new(1, "record.list");
        let json = serialize_message(&req).unwrap();
        assert!(json.contains("\"method\":\"record.list\""));
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn test_response_parsing() {
        let json = r#"{"id":1,"result":{"status":"ok"}}"#;
        let resp: Response = parse_message(json).unwrap();
        let result = resp.into_result().unwrap();
        assert_eq!(result["status"], "ok");
    }

    #[test]
    fn test_cli_hello() {
        let hello = cli_hello();
        assert_eq!(hello.version, PROTOCOL_VERSION);
        assert_eq!(hello.client, CLIENT_NAME);
        assert!(hello.auth_token.is_none());
    }
}
