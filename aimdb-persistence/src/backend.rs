//! Persistence backend trait for AimDB.
//!
//! Defines the [`PersistenceBackend`] trait that concrete implementations
//! (SQLite, Postgres, …) must fulfill.

use core::future::Future;
use core::pin::Pin;

use serde_json::Value;

use crate::error::PersistenceError;

/// Type alias matching the pattern used throughout aimdb-core (no async_trait).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A stored value returned by [`PersistenceBackend::query`].
#[derive(Debug, Clone)]
pub struct StoredValue {
    /// The record name this value belongs to (e.g. `"accuracy.vienna"`) — a
    /// concrete key, never a pattern.
    pub record_name: String,
    /// The JSON-serialized value.
    pub value: Value,
    /// Unix timestamp in milliseconds when the value was persisted.
    pub stored_at: u64,
}

/// Parameters for [`PersistenceBackend::query`].
#[derive(Debug, Clone, Default)]
pub struct QueryParams {
    /// For pattern queries: maximum number of results **per matching record**.
    pub limit_per_record: Option<usize>,
    /// Only return values stored at or after this timestamp (Unix ms).
    pub start_time: Option<u64>,
    /// Only return values stored at or before this timestamp (Unix ms).
    pub end_time: Option<u64>,
}

/// Pluggable persistence backend.
///
/// Implementations run on a concrete async runtime (e.g. Tokio). The trait
/// uses manual `BoxFuture` instead of `async_trait` for consistency with the
/// rest of the AimDB codebase.
pub trait PersistenceBackend: Send + Sync {
    /// Store a JSON value for a record.
    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>>;

    /// Query stored values, with optional pattern and time-range support.
    ///
    /// `record_pattern` is MQTT-style over dot-separated record keys, as
    /// subscriptions use: `*` covers exactly one segment, `#` zero or more, so
    /// `"sensors.*"` matches `"sensors.temp"` but not `"sensors.temp.vienna"`.
    ///
    /// Narrow with [`literal_prefix`](crate::literal_prefix) and decide with
    /// [`topic_matches`](crate::topic_matches) rather than approximating the
    /// grammar in a store-native pattern language: returning keys outside the
    /// pattern leaks records the caller may not be allowed to see.
    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>>;

    /// Initialize storage (create tables, indexes, …).
    ///
    /// Default: no-op. Backends that perform setup eagerly in `::new()`
    /// (like `SqliteBackend`) do not need to override this.
    fn initialize(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    /// Delete all rows older than `older_than` (Unix ms).
    ///
    /// Called automatically by the retention cleanup task registered during
    /// `with_persistence()`. Can also be called explicitly.
    fn cleanup(&self, older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>>;
}
