//! Reusable handler traits for the WebSocket dispatch.
//!
//! The WS server rides `run_session` ([`aimdb_core::session::run_session`]) via
//! [`super::dispatch`]; what lives here is the pluggable application surface the
//! dispatch consumes:
//!
//! - [`QueryHandler`] — answers `record.query` calls from a persistence backend
//!   (result rows are the shared [`QueryRecord`] vocabulary; without a custom
//!   handler the dispatch falls back to the `QueryHandlerFn` that
//!   `aimdb-persistence::with_persistence` registers in Extensions);
//! - [`SnapshotProvider`] — supplies the late-join current values for a
//!   subscription pattern.
//!
//! `record.list` rows are core's [`RecordMetadata`](aimdb_core::remote::RecordMetadata),
//! shared with every other transport; the dispatch only stamps in the schema
//! name core can't resolve.

use core::future::Future;
use core::pin::Pin;

pub use aimdb_core::remote::QueryRecord;
// Re-export so the builder/dispatch can use it easily.
pub use aimdb_core::router::Router;

// ════════════════════════════════════════════════════════════════════
// Query handler
// ════════════════════════════════════════════════════════════════════

/// Boxed future returned by [`QueryHandler::handle_query`].
pub type QueryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(Vec<QueryRecord>, usize), String>> + Send + 'a>>;

/// Trait for handling `record.query` calls from WebSocket clients.
///
/// Implementations typically query a persistence backend and return matching
/// records. The trait is async to support database I/O.
pub trait QueryHandler: Send + Sync + 'static {
    /// Execute a history query and return `(records, total_count)`.
    ///
    /// - `pattern` — topic pattern (MQTT wildcards, `"*"` for all)
    /// - `from` / `to` — time range (inclusive; units are the handler's
    ///   contract — the persistence backend uses milliseconds since Unix epoch)
    /// - `limit` — max records per matching topic
    fn handle_query<'a>(
        &'a self,
        pattern: &'a str,
        from: Option<u64>,
        to: Option<u64>,
        limit: Option<usize>,
    ) -> QueryFuture<'a>;
}

// ════════════════════════════════════════════════════════════════════
// Snapshot provider (late-join)
// ════════════════════════════════════════════════════════════════════

/// Provides the current serialized values covered by a subscription pattern for
/// late-join snapshots (one `(topic, value)` pair per covered record — a
/// wildcard pattern may cover several; an exact topic matches itself).
pub trait SnapshotProvider: Send + Sync + 'static {
    /// Return the latest serialized values for every topic matching `pattern`.
    fn snapshots(&self, pattern: &str) -> Vec<(String, Vec<u8>)>;
}

/// A snapshot provider that always returns nothing (late-join disabled or no data).
pub struct NoSnapshot;

impl SnapshotProvider for NoSnapshot {
    fn snapshots(&self, _pattern: &str) -> Vec<(String, Vec<u8>)> {
        Vec::new()
    }
}
