//! Reusable handler traits for the WebSocket dispatch.
//!
//! The WS server rides `run_session` ([`aimdb_core::session::run_session`]) via
//! [`super::dispatch`]; what lives here is the pluggable application surface the
//! dispatch consumes: [`SnapshotProvider`] supplies the late-join current values
//! for a subscription pattern.
//!
//! `record.query` has no WebSocket-specific hook. It is answered by the
//! `QueryHandlerFn` in the database's Extensions (registered by
//! `aimdb-persistence::with_persistence`), the same handler every transport uses.
//!
//! `record.list` rows are core's [`RecordMetadata`](aimdb_core::remote::RecordMetadata),
//! shared with every other transport; the dispatch only stamps in the schema
//! name core can't resolve.

// ════════════════════════════════════════════════════════════════════
// Snapshot provider (late-join)
// ════════════════════════════════════════════════════════════════════

/// Provides the current serialized values covered by a subscription pattern for
/// late-join snapshots (one `(record id, topic, value)` tuple per covered pair of record - topic
/// — a wildcard pattern may cover several; an exact topic may match several records).
/// The returned snapshots need record id, different clients may have different
/// read permissions to different records
pub trait SnapshotProvider: Send + Sync + 'static {
    /// Return the latest serialized values for every topic matching `pattern`.
    /// As record keys now rule the broadcasting instead of topic,
    /// return must be tuple of (record index, topic, payload, write stamp)
    fn snapshots(&self, pattern: &str) -> Vec<Snapshot>;
}

/// One late-join snapshot: record index, topic, serialized value, and the
/// stamp of the write the value came from (AimX 4.0 `ts`).
pub type Snapshot = (usize, String, Vec<u8>, aimdb_core::Stamp);

/// A snapshot provider that always returns nothing (late-join disabled or no data).
pub struct NoSnapshot;

impl SnapshotProvider for NoSnapshot {
    fn snapshots(&self, _pattern: &str) -> Vec<Snapshot> {
        Vec::new()
    }
}
