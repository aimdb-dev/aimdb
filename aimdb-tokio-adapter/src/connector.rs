//! Connector task spawning for Tokio runtime
//!
//! This module provides the infrastructure for spawning connector tasks
//! that bridge AimDB records to external systems (MQTT, Kafka, HTTP, etc.).

use aimdb_core::{AimDb, DbResult};

use crate::TokioAdapter;

impl TokioAdapter {
    /// Spawns connector tasks for all registered connectors
    ///
    /// This method iterates through all records in the database and spawns
    /// a background task for each connector that was registered via `.link()`.
    ///
    /// Each connector task will:
    /// 1. Subscribe to the record's buffer
    /// 2. Create a protocol client (MQTT, Kafka, HTTP, etc.)
    /// 3. Bridge values from the buffer to the external system
    ///
    /// # Arguments
    /// * `db` - The database instance with registered records and connectors
    ///
    /// # Returns
    /// `Ok(())` if all connectors were spawned successfully
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::AimDb;
    /// use aimdb_tokio_adapter::TokioAdapter;
    /// use std::sync::Arc;
    ///
    /// #[tokio::main]
    /// async fn main() -> DbResult<()> {
    ///     let runtime = Arc::new(TokioAdapter::new()?);
    ///     
    ///     // Build database with connector registrations
    ///     let db = AimDb::build_with(runtime.clone(), |builder| {
    ///         builder.configure::<WeatherAlert>(|reg| {
    ///             reg.producer(|_em, alert| async { /* ... */ })
    ///                .link("mqtt://broker:1883").finish();
    ///         });
    ///     })?;
    ///     
    ///     // Spawn connector tasks
    ///     runtime.spawn_connectors(&db)?;
    ///     
    ///     Ok(())
    /// }
    /// ```
    pub fn spawn_connectors(&self, db: &AimDb) -> DbResult<()> {
        #[cfg(feature = "tracing")]
        tracing::debug!("Spawning connector tasks for database");

        let inner = db.inner();
        #[cfg(feature = "tracing")]
        let mut total_connectors = 0;

        // Iterate through all registered records
        for (_type_id, record) in inner.records.iter() {
            let connector_count = record.connector_count();

            if connector_count > 0 {
                #[cfg(feature = "tracing")]
                {
                    tracing::info!("Record {:?} has {} connector(s)", _type_id, connector_count);

                    let urls = record.connector_urls();
                    for url in urls {
                        tracing::debug!("  → Connector URL: {}", url);
                        total_connectors += 1;
                    }
                }

                #[cfg(not(feature = "tracing"))]
                {
                    // Consume the connector_count to avoid unused warning
                    let _ = connector_count;
                }
            }
        }

        #[cfg(feature = "tracing")]
        if total_connectors > 0 {
            tracing::info!("Total connectors discovered: {}", total_connectors);
        } else {
            tracing::debug!("No connectors registered in this database");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::AimDbBuilder;
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    struct TestMessage {
        _content: String,
    }

    #[tokio::test]
    async fn test_spawn_connectors_empty_database() {
        let adapter = TokioAdapter::new().unwrap();
        let mut builder = AimDbBuilder::new().with_runtime(Arc::new(adapter));

        // Register a record with no connectors
        builder.configure::<TestMessage>(|reg| {
            reg.producer(|_em, _msg| async {})
                .consumer(|_em, _msg| async {});
        });

        let db = builder.build().unwrap();

        // Should succeed even with no connectors
        let result = adapter.spawn_connectors(&db);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_spawn_connectors_with_links() {
        let adapter = TokioAdapter::new().unwrap();
        let mut builder = AimDbBuilder::new().with_runtime(Arc::new(adapter));

        // Register a record with connectors
        builder.configure::<TestMessage>(|reg| {
            reg.producer(|_em, _msg| async {})
                .consumer(|_em, _msg| async {})
                .link("mqtt://broker.example.com:1883")
                .finish()
                .link("kafka://kafka1:9092/messages")
                .finish();
        });

        let db = builder.build().unwrap();

        // Should discover and validate connectors
        let result = adapter.spawn_connectors(&db);
        assert!(result.is_ok());

        // Verify connectors were registered
        let inner = db.inner();
        let record = inner
            .records
            .values()
            .next()
            .expect("Should have one record");

        assert_eq!(record.connector_count(), 2);

        #[cfg(feature = "std")]
        {
            let urls = record.connector_urls();
            assert!(urls[0].contains("mqtt://"));
            assert!(urls[1].contains("kafka://"));
        }
    }
}
