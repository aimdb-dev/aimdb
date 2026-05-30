//! Type-erased persistence query handler for the AimX `record.query` method.
//!
//! Kept free of persistence-specific imports so `aimdb-core` need not depend on
//! `aimdb-persistence`: the handler is a boxed async function registered in the
//! database's `Extensions` TypeMap by `aimdb_persistence::with_persistence()`,
//! and invoked by the AimX server dispatch when a client calls `record.query`.

/// Type-erased query handler registered by `aimdb-persistence` via Extensions.
///
/// A boxed async function that accepts query parameters (record pattern, limit,
/// start/end timestamps) and returns a JSON value with the results.
pub type QueryHandlerFn = Box<
    dyn Fn(
            QueryHandlerParams,
        ) -> core::pin::Pin<
            Box<dyn core::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync,
>;

/// Parameters for the type-erased query handler.
#[derive(Debug, Clone)]
pub struct QueryHandlerParams {
    /// Record pattern (supports `*` wildcard).
    pub name: String,
    /// Maximum results per matching record.
    pub limit: Option<usize>,
    /// Optional start timestamp (Unix ms).
    pub start: Option<u64>,
    /// Optional end timestamp (Unix ms).
    pub end: Option<u64>,
}
