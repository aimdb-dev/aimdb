//! Reusable handler traits for the WebSocket dispatch (Phase 4).
//!
//! The hand-rolled per-connection recv/send loops that used to live here were
//! retired in Phase 4 — the WS server now rides `run_session`
//! ([`aimdb_core::session::run_session`]) via [`crate::dispatch`]. What survives
//! is the pluggable application surface the dispatch consumes:
//!
//! - [`QueryHandler`] — answers client `query` messages from a persistence backend;
//! - [`SnapshotProvider`] — supplies the late-join current value for a topic.

use core::future::Future;
use core::pin::Pin;

use crate::protocol::QueryRecord;

// Re-export so the builder/dispatch can use it easily.
pub use aimdb_core::router::Router;

// ════════════════════════════════════════════════════════════════════
// Query handler
// ════════════════════════════════════════════════════════════════════

/// Boxed future returned by [`QueryHandler::handle_query`].
pub type QueryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(Vec<QueryRecord>, usize), String>> + Send + 'a>>;

/// Trait for handling `Query` messages from WebSocket clients.
///
/// Implementations typically query a persistence backend and return matching
/// records. The trait is async to support database I/O.
pub trait QueryHandler: Send + Sync + 'static {
    /// Execute a history query and return `(records, total_count)`.
    ///
    /// - `pattern` — topic pattern (MQTT wildcards, `"*"` for all)
    /// - `from` / `to` — time range in **milliseconds** since Unix epoch (inclusive)
    /// - `limit` — max records per matching topic
    fn handle_query<'a>(
        &'a self,
        pattern: &'a str,
        from: Option<u64>,
        to: Option<u64>,
        limit: Option<usize>,
    ) -> QueryFuture<'a>;
}

/// A query handler that always returns an error (used when no persistence
/// backend is configured).
pub struct NoQuery;

impl QueryHandler for NoQuery {
    fn handle_query<'a>(
        &'a self,
        _pattern: &'a str,
        _from: Option<u64>,
        _to: Option<u64>,
        _limit: Option<usize>,
    ) -> QueryFuture<'a> {
        Box::pin(
            async move { Err("Query not supported — no persistence backend configured".into()) },
        )
    }
}

// ════════════════════════════════════════════════════════════════════
// Snapshot provider (late-join)
// ════════════════════════════════════════════════════════════════════

/// Provides the current serialized value of a record for late-join snapshots.
pub trait SnapshotProvider: Send + Sync + 'static {
    /// Return the latest serialized value for the given topic, if available.
    fn snapshot(&self, topic: &str) -> Option<Vec<u8>>;
}

/// A snapshot provider that always returns `None` (late-join disabled or no data).
pub struct NoSnapshot;

impl SnapshotProvider for NoSnapshot {
    fn snapshot(&self, _topic: &str) -> Option<Vec<u8>> {
        None
    }
}
