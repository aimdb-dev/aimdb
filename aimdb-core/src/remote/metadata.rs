//! Record metadata types for remote introspection (feature `remote`).
//!
//! [`RecordMetadata`] describes a registered record's runtime state — buffer
//! configuration, producer/consumer counts, the `writable` flag, and
//! (feature-gated) buffer metrics / stage profiling — for the AimX `record.list`
//! response. Computed on demand from a record's static structure; core keeps no
//! per-record metadata state.

use alloc::format;
use alloc::string::{String, ToString};
#[cfg(feature = "observability")]
use alloc::vec::Vec;
use core::any::TypeId;
use serde::{Deserialize, Serialize};

use crate::graph::RecordOrigin;
use crate::record_id::{RecordId, RecordKey};

/// Metadata about a registered record type
///
/// Provides information for remote introspection, including buffer
/// configuration and producer/consumer counts.
///
/// When the `observability` feature is enabled, additional fields are included
/// for buffer-level statistics (produced_count, consumed_count, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
    /// Unique record identifier (index in the storage)
    pub record_id: u32,

    /// Unique record key (stable identifier for lookup)
    pub record_key: String,

    /// Record type name (Rust type name)
    pub name: String,

    /// TypeId as hexadecimal string
    pub type_id: String,

    /// How the record gets its values (Source, Link, Transform, Passive)
    pub origin: RecordOrigin,

    /// Buffer type: "spmc_ring", "single_latest", "mailbox", or "none"
    pub buffer_type: String,

    /// Buffer capacity (None for unbounded or no buffer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_capacity: Option<usize>,

    /// Number of registered producer services
    pub producer_count: usize,

    /// Number of registered consumer services
    pub consumer_count: usize,

    /// Whether write operations are permitted for this record
    pub writable: bool,

    /// Number of outbound connector links registered
    pub outbound_connector_count: usize,

    /// Data-contract schema name (e.g. `"temperature"`), when the serving
    /// dispatch owns a schema registry that can resolve it (the WebSocket
    /// connector's `StreamableRegistry`). Core alone cannot map a `TypeId` to a
    /// contract name and leaves it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,

    /// Entity / node identifier (e.g. `"vienna"` for `"temp.vienna"`), derived
    /// from the record key's final `.` segment. The server is the authority on
    /// naming conventions — clients use this field instead of parsing keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,

    // ===== Buffer metrics (feature-gated) =====
    /// Total items pushed to the buffer (metrics feature only)
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub produced_count: Option<u64>,

    /// Total items consumed from the buffer (metrics feature only)
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_count: Option<u64>,

    /// Total items dropped due to overflow/lag (metrics feature only)
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,

    /// Current buffer occupancy: (items, capacity) (metrics feature only)
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occupancy: Option<(usize, usize)>,

    // ===== Stage profiling (feature-gated) =====
    /// Per-stage timing metrics (`.source()`/`.tap()`/`.link()`), if the
    /// `observability` feature is enabled and any stage has been registered.
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_profiling: Option<Vec<crate::profiling::StageProfilingInfo>>,

    // ===== Signal gauges (feature-gated) =====
    /// Per-record domain-signal statistics (last/min/max/mean) fed by
    /// `Observable::observe()`, if the `observability` feature is enabled and any
    /// gauge has been registered.
    #[cfg(feature = "observability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_stats: Option<Vec<crate::profiling::SignalStatsInfo>>,
}

impl RecordMetadata {
    /// Creates a new record metadata entry
    ///
    /// # Arguments
    /// * `record_id` - The RecordId index
    /// * `record_key` - The unique record key
    /// * `type_id` - The TypeId of the record
    /// * `name` - The Rust type name
    /// * `origin` - How the record gets its values (Source, Link, Transform, Passive)
    /// * `buffer_type` - Buffer type string
    /// * `buffer_capacity` - Optional buffer capacity
    /// * `producer_count` - Number of producers
    /// * `consumer_count` - Number of consumers
    /// * `writable` - Whether writes are permitted
    /// * `outbound_connector_count` - Number of outbound connectors
    #[allow(clippy::too_many_arguments)]
    pub fn new<K: RecordKey>(
        record_id: RecordId,
        record_key: K,
        type_id: TypeId,
        name: String,
        origin: RecordOrigin,
        buffer_type: String,
        buffer_capacity: Option<usize>,
        producer_count: usize,
        consumer_count: usize,
        writable: bool,
        outbound_connector_count: usize,
    ) -> Self {
        let entity = record_key
            .as_str()
            .rsplit('.')
            .next()
            .map(|s| s.to_string());
        Self {
            record_id: record_id.raw(),
            record_key: record_key.as_str().to_string(),
            name,
            type_id: format!("{:?}", type_id),
            origin,
            buffer_type,
            buffer_capacity,
            producer_count,
            consumer_count,
            writable,
            outbound_connector_count,
            schema_type: None,
            entity,
            #[cfg(feature = "observability")]
            produced_count: None,
            #[cfg(feature = "observability")]
            consumed_count: None,
            #[cfg(feature = "observability")]
            dropped_count: None,
            #[cfg(feature = "observability")]
            occupancy: None,
            #[cfg(feature = "observability")]
            stage_profiling: None,
            #[cfg(feature = "observability")]
            signal_stats: None,
        }
    }

    /// Sets buffer metrics from a snapshot (metrics feature only)
    ///
    /// Populates produced_count, consumed_count, dropped_count, and occupancy
    /// from the provided metrics snapshot.
    #[cfg(feature = "observability")]
    pub fn with_buffer_metrics(mut self, snapshot: crate::buffer::BufferMetricsSnapshot) -> Self {
        self.produced_count = Some(snapshot.produced_count);
        self.consumed_count = Some(snapshot.consumed_count);
        self.dropped_count = Some(snapshot.dropped_count);
        // Only include occupancy if it's meaningful (non-zero capacity)
        if snapshot.occupancy.1 > 0 {
            self.occupancy = Some(snapshot.occupancy);
        }
        self
    }

    /// Attaches a stage profiling snapshot (profiling feature only).
    #[cfg(feature = "observability")]
    pub fn with_stage_profiling(
        mut self,
        stages: Vec<crate::profiling::StageProfilingInfo>,
    ) -> Self {
        if !stages.is_empty() {
            self.stage_profiling = Some(stages);
        }
        self
    }

    /// Attaches a signal-gauge snapshot (observability feature only).
    #[cfg(feature = "observability")]
    pub fn with_signal_stats(mut self, signals: Vec<crate::profiling::SignalStatsInfo>) -> Self {
        if !signals.is_empty() {
            self.signal_stats = Some(signals);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_id::StringKey;

    #[test]
    fn test_record_metadata_creation() {
        let type_id = TypeId::of::<i32>();
        let metadata = RecordMetadata::new(
            RecordId::new(0),
            StringKey::new("test.record"),
            type_id,
            "i32".to_string(),
            RecordOrigin::Source,
            "spmc_ring".to_string(),
            Some(100),
            1,
            2,
            false,
            0,
        );

        assert_eq!(metadata.record_id, 0);
        assert_eq!(metadata.record_key, "test.record");
        assert_eq!(metadata.name, "i32");
        assert!(matches!(metadata.origin, RecordOrigin::Source));
        assert_eq!(metadata.buffer_type, "spmc_ring");
        assert_eq!(metadata.buffer_capacity, Some(100));
        assert_eq!(metadata.producer_count, 1);
        assert_eq!(metadata.consumer_count, 2);
        assert_eq!(metadata.outbound_connector_count, 0);
        assert!(!metadata.writable);
    }

    #[test]
    fn test_record_metadata_serialization() {
        let type_id = TypeId::of::<String>();
        let metadata = RecordMetadata::new(
            RecordId::new(1),
            StringKey::new("app.config"),
            type_id,
            "String".to_string(),
            RecordOrigin::Passive,
            "single_latest".to_string(),
            None,
            1,
            1,
            true,
            2,
        );

        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("\"record_id\":1"));
        assert!(json.contains("\"record_key\":\"app.config\""));
        assert!(json.contains("\"name\":\"String\""));
        assert!(json.contains("\"origin\":\"passive\""));
        assert!(json.contains("\"buffer_type\":\"single_latest\""));
        assert!(json.contains("\"writable\":true"));
        assert!(json.contains("\"outbound_connector_count\":2"));
    }
}
