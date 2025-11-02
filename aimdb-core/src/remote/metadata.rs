//! Record metadata types for remote introspection

use core::any::TypeId;
use serde::{Deserialize, Serialize};
use std::string::String;

/// Metadata about a registered record type
///
/// Provides information for remote introspection, including buffer
/// configuration, producer/consumer counts, and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
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

    /// Number of connector links registered
    pub connector_count: usize,
}

impl RecordMetadata {
    /// Creates a new record metadata entry
    ///
    /// # Arguments
    /// * `type_id` - The TypeId of the record
    /// * `name` - The Rust type name
    /// * `buffer_type` - Buffer type string
    /// * `buffer_capacity` - Optional buffer capacity
    /// * `producer_count` - Number of producers
    /// * `consumer_count` - Number of consumers
    /// * `writable` - Whether writes are permitted
    /// * `created_at` - Creation timestamp (ISO 8601)
    /// * `connector_count` - Number of connectors
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        type_id: TypeId,
        name: String,
        buffer_type: String,
        buffer_capacity: Option<usize>,
        producer_count: usize,
        consumer_count: usize,
        writable: bool,
        created_at: String,
        connector_count: usize,
    ) -> Self {
        Self {
            name,
            type_id: format!("{:?}", type_id),
            buffer_type,
            buffer_capacity,
            producer_count,
            consumer_count,
            writable,
            created_at,
            last_update: None,
            connector_count,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_metadata_creation() {
        let type_id = TypeId::of::<i32>();
        let metadata = RecordMetadata::new(
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

        assert_eq!(metadata.name, "i32");
        assert_eq!(metadata.buffer_type, "spmc_ring");
        assert_eq!(metadata.buffer_capacity, Some(100));
        assert_eq!(metadata.producer_count, 1);
        assert_eq!(metadata.consumer_count, 2);
        assert!(!metadata.writable);
        assert_eq!(metadata.connector_count, 0);
    }

    #[test]
    fn test_record_metadata_serialization() {
        let type_id = TypeId::of::<String>();
        let metadata = RecordMetadata::new(
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
        assert!(json.contains("\"name\":\"String\""));
        assert!(json.contains("\"buffer_type\":\"single_latest\""));
        assert!(json.contains("\"writable\":true"));
        assert!(json.contains("\"connector_count\":2"));
    }
}
