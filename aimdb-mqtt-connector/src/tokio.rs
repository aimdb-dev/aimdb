//! Tokio-based MQTT connector implementation
//!
//! This module provides MQTT connectivity using `rumqttc` for std environments.

use crate::MqttConfig;
use aimdb_core::{AimDb, AnyRecord, DbError, DbResult};
use rumqttc::{AsyncClient, MqttOptions};
use std::sync::Arc;
use std::time::Duration;

/// Spawn MQTT connector tasks for all records with mqtt:// URLs
///
/// This function is called by `TokioAdapter::spawn_connectors()` to create
/// background tasks that publish record updates to MQTT brokers.
///
/// # Arguments
/// * `db` - The database instance with registered records
///
/// # Returns
/// * `Ok(usize)` - Number of MQTT connectors spawned
/// * `Err(DbError)` - If connector spawning fails
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_mqtt_connector::tokio::spawn_mqtt_connectors;
/// 
/// let db = AimDb::build_with(runtime.clone(), |builder| {
///     builder.configure::<Temperature>(|reg| {
///         reg.link("mqtt://broker:1883/sensors/temp").finish();
///     });
/// })?;
///
/// let connector_count = spawn_mqtt_connectors(&db)?;
/// println!("Spawned {} MQTT connectors", connector_count);
/// ```
pub fn spawn_mqtt_connectors(db: &AimDb) -> DbResult<usize> {
    #[cfg(feature = "tracing")]
    tracing::debug!("Spawning MQTT connector tasks");

    let mut spawned_count = 0;
    let inner = db.inner();

    // Iterate through all registered records
    for (type_id, record) in inner.records.iter() {
        let urls = record.connector_urls();

        for url_str in urls {
            // Only process mqtt:// and mqtts:// URLs
            if url_str.starts_with("mqtt://") || url_str.starts_with("mqtts://") {
                #[cfg(feature = "tracing")]
                tracing::info!(
                    "Spawning MQTT connector for record {:?}: {}",
                    type_id,
                    url_str
                );

                // Parse URL and create config
                let url = aimdb_core::connector::ConnectorUrl::parse(&url_str).map_err(|e| {
                    DbError::InvalidOperation {
                        operation: "parse_mqtt_url".to_string(),
                        reason: format!("Invalid MQTT URL: {}", e),
                    }
                })?;

                let config = MqttConfig::from_url(url).map_err(|e| DbError::InvalidOperation {
                    operation: "create_mqtt_config".to_string(),
                    reason: format!("MQTT config error: {}", e),
                })?;

                // Spawn the connector task
                spawn_mqtt_task(config, record.as_ref())?;
                spawned_count += 1;
            }
        }
    }

    #[cfg(feature = "tracing")]
    if spawned_count > 0 {
        tracing::info!("Spawned {} MQTT connector task(s)", spawned_count);
    } else {
        tracing::debug!("No MQTT connectors to spawn");
    }

    Ok(spawned_count)
}

/// Spawn a single MQTT connector task for a specific record
fn spawn_mqtt_task(config: MqttConfig, _record: &dyn AnyRecord) -> DbResult<()> {
    // Create MQTT client
    let mut mqtt_opts = MqttOptions::new(
        config.client_id.clone().unwrap_or_else(|| {
            format!("aimdb-{}", uuid::Uuid::new_v4())
        }),
        config.url.host.clone(),
        config.url.effective_port().unwrap_or(1883),
    );

    mqtt_opts.set_keep_alive(Duration::from_secs(30));

    // Add credentials if provided
    if let (Some(ref username), Some(ref password)) = (&config.url.username, &config.url.password)
    {
        mqtt_opts.set_credentials(username, password);
    }

    let (client, mut event_loop) = AsyncClient::new(mqtt_opts, 10);
    let client = Arc::new(client);

    // Spawn event loop task (required by rumqttc)
    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::debug!("MQTT event loop started");

        loop {
            match event_loop.poll().await {
                Ok(_notification) => {
                    // Event loop is running
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("MQTT event loop error: {:?}", e);
                    
                    // Wait before reconnecting
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // TODO: Spawn publisher task that subscribes to buffer and publishes
    // This requires:
    // 1. Access to the record's buffer/emitter
    // 2. Subscription mechanism
    // 3. Serialization of values to MQTT payload
    //
    // For now, we just create the client and event loop
    
    #[cfg(feature = "tracing")]
    tracing::info!(
        "MQTT connector spawned for topic '{}' (client ready, awaiting buffer integration)",
        config.topic
    );

    // TODO: Store client for later use in publisher task
    let _ = client; // Silence unused warning for now

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::TokioAdapter;

    #[derive(Clone, Debug)]
    struct TestMessage {
        content: String,
    }

    #[tokio::test]
    async fn test_spawn_mqtt_connectors_empty() {
        let adapter = TokioAdapter::new().unwrap();
        let mut builder = AimDbBuilder::new().with_runtime(Arc::new(adapter));

        // Register record with no connectors
        builder.configure::<TestMessage>(|reg| {
            reg.producer(|_em, _msg| async {})
               .consumer(|_em, _msg| async {}); // Need consumer for validation
        });

        let db = builder.build().unwrap();

        // Should return 0 connectors spawned
        let result = spawn_mqtt_connectors(&db);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_spawn_mqtt_connectors_non_mqtt_url() {
        let adapter = TokioAdapter::new().unwrap();
        let mut builder = AimDbBuilder::new().with_runtime(Arc::new(adapter));

        // Register record with non-MQTT connector
        builder.configure::<TestMessage>(|reg| {
            reg.producer(|_em, _msg| async {})
               .consumer(|_em, _msg| async {}) // Need consumer for validation
               .link("kafka://kafka1:9092/topic")
               .finish();
        });

        let db = builder.build().unwrap();

        // Should skip non-MQTT URLs
        let result = spawn_mqtt_connectors(&db);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    // Note: We can't test actual MQTT connection without a broker
    // Integration tests with testcontainers will be added separately
}
