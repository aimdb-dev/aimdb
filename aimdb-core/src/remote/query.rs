//! Type-erased persistence query handler for the AimX `record.query` method.
//!
//! Kept free of persistence-specific imports so `aimdb-core` need not depend on
//! `aimdb-persistence`: the handler is a boxed async function registered in the
//! database's `Extensions` TypeMap by `aimdb_persistence::with_persistence()`,
//! and invoked by the AimX server dispatch when a client calls `record.query`.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// Type-erased query handler registered by `aimdb-persistence` via Extensions.
///
/// A boxed async function that accepts query parameters (record pattern, limit,
/// start/end timestamps) and returns the matching rows, sorted by `ts`
/// ascending. Every transport replies to `record.query` with `{"records": [...]}`.
pub type QueryHandlerFn = Box<
    dyn Fn(
            QueryHandlerParams,
        ) -> core::pin::Pin<
            Box<dyn core::future::Future<Output = Result<Vec<QueryRecord>, String>> + Send>,
        > + Send
        + Sync,
>;

/// One row of a `record.query` result — the canonical shape every transport
/// shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRecord {
    /// Record key / topic the value was stored under (e.g. `"temp.vienna"`).
    pub topic: String,
    /// Deserialized record value.
    pub payload: serde_json::Value,
    /// Storage timestamp (milliseconds since Unix epoch).
    pub ts: u64,
}

/// The pattern a `record.query` with no `name` resolves to: every record, at
/// every depth.
pub const QUERY_ALL_PATTERN: &str = "#";

/// Rows per record a `record.query` with no `limit` returns: the newest one.
pub const DEFAULT_QUERY_LIMIT: usize = 1;

/// Parameters for the type-erased query handler, fully resolved: every
/// transport builds them with [`QueryHandlerParams::from_json`], which applies
/// the defaults, so every handler sees the same values for the same request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryHandlerParams {
    /// Record pattern, MQTT-style: `*` matches one dot-separated segment, `#`
    /// zero or more. Defaults to [`QUERY_ALL_PATTERN`].
    pub name: String,
    /// Maximum rows per matching record (the newest ones). Defaults to
    /// [`DEFAULT_QUERY_LIMIT`].
    pub limit: usize,
    /// Optional start timestamp (Unix ms), inclusive.
    pub start: Option<u64>,
    /// Optional end timestamp (Unix ms), inclusive.
    pub end: Option<u64>,
}

impl QueryHandlerParams {
    /// Parse `record.query` params (`{name, limit, start, end}`), applying the
    /// defaults. A missing field, a field of the wrong type, or non-object
    /// params all fall back to the default.
    pub fn from_json(params: &serde_json::Value) -> Self {
        Self {
            name: params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(QUERY_ALL_PATTERN)
                .to_string(),
            limit: params
                .get("limit")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(DEFAULT_QUERY_LIMIT),
            start: params.get("start").and_then(|v| v.as_u64()),
            end: params.get("end").and_then(|v| v.as_u64()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_params_get_the_defaults() {
        for params in [json!({}), json!(null)] {
            assert_eq!(
                QueryHandlerParams::from_json(&params),
                QueryHandlerParams {
                    name: QUERY_ALL_PATTERN.into(),
                    limit: DEFAULT_QUERY_LIMIT,
                    start: None,
                    end: None,
                }
            );
        }
    }

    #[test]
    fn explicit_params_pass_through() {
        let params = json!({"name": "sensors.*", "limit": 10, "start": 100, "end": 200});
        assert_eq!(
            QueryHandlerParams::from_json(&params),
            QueryHandlerParams {
                name: "sensors.*".into(),
                limit: 10,
                start: Some(100),
                end: Some(200),
            }
        );
    }

    #[test]
    fn wrongly_typed_params_fall_back_to_the_defaults() {
        let params = json!({"name": 5, "limit": "10", "start": "a", "end": -1});
        assert_eq!(
            QueryHandlerParams::from_json(&params),
            QueryHandlerParams::from_json(&json!({}))
        );
    }
}
