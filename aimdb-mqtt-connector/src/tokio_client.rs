//! MQTT client management and lifecycle
//!
//! This module provides a client pool that:
//! - Manages a single MQTT broker connection
//! - Automatic event loop spawning
//! - Thread-safe access from multiple consumers
//! - Explicit lifecycle management (user controls when clients are created)

use aimdb_core::connector::ConnectorUrl;
use rumqttc::{AsyncClient, EventLoop, MqttOptions};
use std::sync::Arc;
use std::time::Duration;

/// MQTT connector for a single broker connection
///
/// Each connector manages ONE MQTT broker connection. For multiple brokers,
/// create multiple connectors and register them with different schemes.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_mqtt_connector::MqttConnector;
///
/// // Create connector for a specific broker
/// let connector = MqttConnector::new("mqtt://localhost:1883").await?;
///
/// // Register with database
/// let db = AimDbBuilder::new()
///     .with_connector("mqtt", Arc::new(connector))
///     .build()?;
/// ```
pub struct MqttConnector {
    client: Arc<AsyncClient>,
}

impl MqttConnector {
    /// Create a new MQTT connector for a specific broker
    ///
    /// Creates an MQTT client and spawns its event loop immediately.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    ///   Note: The URL should NOT include a topic - just the broker address
    ///
    /// # Returns
    /// * `Ok(connector)` if connection was created successfully
    /// * `Err(_)` if URL is invalid
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let connector = MqttConnector::new("mqtt://localhost:1883").await?;
    /// let connector_secure = MqttConnector::new("mqtts://cloud.example.com:8883").await?;
    /// ```
    pub async fn new(broker_url: &str) -> Result<Self, String> {
        // Parse the broker URL - we accept it with or without a topic
        let mut url = broker_url.to_string();

        // If no topic is provided, add a dummy one for parsing
        if !url.contains('/') || url.matches('/').count() < 3 {
            url = format!("{}/dummy", url.trim_end_matches('/'));
        }

        let connector_url =
            ConnectorUrl::parse(&url).map_err(|e| format!("Invalid MQTT URL: {}", e))?;

        let host = connector_url.host.clone();
        let port = connector_url.port.unwrap_or_else(|| {
            if connector_url.scheme == "mqtts" {
                8883
            } else {
                1883
            }
        });

        let broker_key = format!("{}:{}", host, port);

        #[cfg(feature = "tracing")]
        tracing::info!("Creating MQTT client for {}", broker_key);

        let client_id = format!("aimdb-{}", uuid::Uuid::new_v4());

        let mut mqtt_opts = MqttOptions::new(client_id, host, port);

        mqtt_opts.set_keep_alive(Duration::from_secs(30));

        // Add credentials if provided
        if let (Some(ref username), Some(ref password)) =
            (&connector_url.username, &connector_url.password)
        {
            mqtt_opts.set_credentials(username, password);
        }

        // Create client and event loop
        let (client, event_loop) = AsyncClient::new(mqtt_opts, 10);

        // Spawn event loop task (required by rumqttc)
        spawn_event_loop(event_loop, broker_key);

        Ok(Self {
            client: Arc::new(client),
        })
    }
}

// Implement the connector trait from aimdb-core
impl aimdb_core::transport::Connector for MqttConnector {
    fn publish(
        &self,
        destination: &str,
        config: &aimdb_core::transport::ConnectorConfig,
        payload: &[u8],
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<Output = Result<(), aimdb_core::transport::PublishError>>
                + Send
                + '_,
        >,
    > {
        use aimdb_core::transport::PublishError;

        // Extract topic from destination (destination is already the topic)
        let topic = destination.to_string();
        let payload_owned = payload.to_vec();
        let qos = config.qos;
        let retain = config.retain;
        let client = self.client.clone();

        Box::pin(async move {
            // Determine QoS
            let qos_level = match qos {
                0 => rumqttc::QoS::AtMostOnce,
                1 => rumqttc::QoS::AtLeastOnce,
                2 => rumqttc::QoS::ExactlyOnce,
                _ => return Err(PublishError::UnsupportedQoS),
            };

            // Publish the message
            #[cfg(feature = "tracing")]
            let topic_for_log = topic.clone();

            client
                .publish(topic, qos_level, retain, payload_owned)
                .await
                .map_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::error!("MQTT publish failed: {}", _e);

                    PublishError::ConnectionFailed
                })?;

            #[cfg(feature = "tracing")]
            tracing::debug!("Published to topic: {}", topic_for_log);
            Ok(())
        })
    }
}

/// Spawn the MQTT event loop in a background task
///
/// The event loop is required by rumqttc to handle:
/// - Network I/O (reading/writing packets)
/// - Reconnection logic
/// - QoS handshakes
///
/// # Arguments
/// * `event_loop` - The rumqttc EventLoop to run
/// * `_broker_key` - Broker identifier for logging (unused in release builds)
fn spawn_event_loop(mut event_loop: EventLoop, _broker_key: String) {
    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::debug!("MQTT event loop started for {}", _broker_key);

        loop {
            match event_loop.poll().await {
                Ok(_notification) => {
                    // Event loop is running normally
                    // Notifications include: Incoming publishes, connection status, etc.
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("MQTT event loop error for {}: {:?}", _broker_key, _e);

                    // Wait before reconnecting
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connector_creation() {
        let connector = MqttConnector::new("mqtt://localhost:1883").await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let connector = MqttConnector::new("mqtt://broker.local:9999").await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let connector = MqttConnector::new("not-a-valid-url").await;
        assert!(connector.is_err());
    }
}
