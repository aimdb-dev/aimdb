//! Record metadata types for remote introspection

use core::any::TypeId;
use serde::{Deserialize, Serialize};
use std::string::String;

use crate::record_id::{RecordId, RecordKey};

/// Metadata about a registered record type
///
/// Provides information for remote introspection, including buffer
/// configuration, producer/consumer counts, and timestamps.
///
/// When the `metrics` feature is enabled, additional fields are included
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

    /// When the record was registered (ISO 8601)
    pub created_at: String,

    /// Last update timestamp (ISO 8601), None if never updated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,

    /// Number of outbound connector links registered
    pub outbound_connector_count: usize,

    // ===== Buffer metrics (feature-gated) =====
    /// Total items pushed to the buffer (metrics feature only)
    #[cfg(feature = "metrics")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub produced_count: Option<u64>,

    /// Total items consumed from the buffer (metrics feature only)
    #[cfg(feature = "metrics")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_count: Option<u64>,

    /// Total items dropped due to overflow/lag (metrics feature only)
    #[cfg(feature = "metrics")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,

    /// Current buffer occupancy: (items, capacity) (metrics feature only)
    #[cfg(feature = "metrics")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occupancy: Option<(usize, usize)>,
}

impl RecordMetadata {
    /// Creates a new record metadata entry
    ///
    /// # Arguments
    /// * `record_id` - The RecordId index
    /// * `record_key` - The unique record key
    /// * `type_id` - The TypeId of the record
    /// * `name` - The Rust type name
    /// * `buffer_type` - Buffer type string
    /// * `buffer_capacity` - Optional buffer capacity
    /// * `producer_count` - Number of producers
    /// * `consumer_count` - Number of consumers
    /// * `writable` - Whether writes are permitted
    /// * `created_at` - Creation timestamp (ISO 8601)
    /// * `outbound_connector_count` - Number of outbound connectors
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        record_id: RecordId,
        record_key: RecordKey,
        type_id: TypeId,
        name: String,
        buffer_type: String,
        buffer_capacity: Option<usize>,
        producer_count: usize,
        consumer_count: usize,
        writable: bool,
        created_at: String,
        outbound_connector_count: usize,
    ) -> Self {
        Self {
            record_id: record_id.raw(),
            record_key: record_key.as_str().to_string(),
            name,
            type_id: format!("{:?}", type_id),
            buffer_type,
            buffer_capacity,
            producer_count,
            consumer_count,
            writable,
            created_at,
            last_update: None,
            outbound_connector_count,
            #[cfg(feature = "metrics")]
            produced_count: None,
            #[cfg(feature = "metrics")]
            consumed_count: None,
            #[cfg(feature = "metrics")]
            dropped_count: None,
            #[cfg(feature = "metrics")]
            occupancy: None,
        }
    }

    /// Sets the last update timestamp
    pub fn with_last_update(mut self, timestamp: String) -> Self {
        self.last_update = Some(timestamp);
        self
    }

    /// Sets the last update timestamp from an Option
    pub fn with_last_update_opt(mut self, timestamp: Option<String>) -> Self {
        self.last_update = timestamp;
        self
    }

    /// Sets buffer metrics from a snapshot (metrics feature only)
    ///
    /// Populates produced_count, consumed_count, dropped_count, and occupancy
    /// from the provided metrics snapshot.
    #[cfg(feature = "metrics")]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_metadata_creation() {
        let type_id = TypeId::of::<i32>();
        let metadata = RecordMetadata::new(
            RecordId::new(0),
            RecordKey::new("test.record"),
            type_id,
            "i32".to_string(),
            "spmc_ring".to_string(),
            Some(100),
            1,
            2,
            false,
            "2025-10-31T10:00:00.000Z".to_string(),
            0,
        );

        assert_eq!(metadata.record_id, 0);
        assert_eq!(metadata.record_key, "test.record");
        assert_eq!(metadata.name, "i32");
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
            RecordKey::new("app.config"),
            type_id,
            "String".to_string(),
            "single_latest".to_string(),
            None,
            1,
            1,
            true,
            "2025-10-31T10:00:00.000Z".to_string(),
            2,
        )
        .with_last_update("2025-10-31T12:00:00.000Z".to_string());

        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("\"record_id\":1"));
        assert!(json.contains("\"record_key\":\"app.config\""));
        assert!(json.contains("\"name\":\"String\""));
        assert!(json.contains("\"buffer_type\":\"single_latest\""));
        assert!(json.contains("\"writable\":true"));
        assert!(json.contains("\"outbound_connector_count\":2"));
    }
}
