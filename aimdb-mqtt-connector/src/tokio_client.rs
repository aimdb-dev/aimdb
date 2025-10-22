//! MQTT client management and lifecycle
//!
//! This module provides a client pool that ensures:
//! - Single client per broker (no duplicate connections)
//! - Automatic event loop spawning
//! - Thread-safe access from multiple consumers
//! - Explicit lifecycle management (user controls when clients are created)

use crate::MqttConfig;
use aimdb_core::connector::ConnectorUrl;
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// MQTT client pool for managing broker connections
///
/// Users create this pool, add brokers, and pass it to `spawn_mqtt_connectors()`.
/// This provides explicit dependency injection and full control over client lifecycle.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_mqtt_connector::MqttClientPool;
///
/// // Create pool and add brokers
/// let pool = MqttClientPool::new();
/// pool.add_broker("mqtt://broker1:1883").await?;
/// pool.add_broker("mqtts://secure-broker:8883").await?;
///
/// // Pass to connector spawner
/// spawn_mqtt_connectors(&db, &pool)?;
/// ```
pub struct MqttClientPool {
    clients: Arc<Mutex<HashMap<String, Arc<MqttClient>>>>,
}

impl MqttClientPool {
    /// Create a new empty client pool
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Add a broker to the pool
    ///
    /// Creates and caches an MQTT client for the specified broker URL.
    /// If a client already exists for this broker, returns immediately.
    ///
    /// # Arguments
    /// * `url` - Broker URL (mqtt://host:port or mqtts://host:port)
    ///
    /// # Returns
    /// * `Ok(())` if client was created or already exists
    /// * `Err(_)` if URL is invalid or client creation fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pool = MqttClientPool::new();
    /// pool.add_broker("mqtt://localhost:1883").await?;
    /// pool.add_broker("mqtts://cloud.example.com:8883").await?;
    /// ```
    pub async fn add_broker(&self, url: &str) -> Result<(), String> {
        let connector_url = ConnectorUrl::parse(url)
            .map_err(|e| format!("Invalid MQTT URL: {}", e))?;
        
        let config = MqttConfig::from_url(connector_url)
            .map_err(|e| format!("Invalid MQTT config: {}", e))?;
        
        self.get_or_create_client(&config);
        Ok(())
    }

    /// Get or create a client for a specific configuration
    ///
    /// Internal method used by spawn_mqtt_connectors() to get clients.
    /// Creates client on first call, returns cached client on subsequent calls.
    pub(crate) fn get_or_create_client(&self, config: &MqttConfig) -> Arc<MqttClient> {
        let broker_key = format!(
            "{}:{}",
            config.url.host,
            config.url.effective_port().unwrap_or(1883)
        );

        let mut clients = self.clients.lock().unwrap();

        // Return existing client if already created
        if let Some(client) = clients.get(&broker_key) {
            #[cfg(feature = "tracing")]
            tracing::debug!("Reusing existing MQTT client for {}", broker_key);
            return client.clone();
        }

        // Create new client
        #[cfg(feature = "tracing")]
        tracing::info!("Creating new MQTT client for {}", broker_key);

        let client_id = config.client_id.clone().unwrap_or_else(|| {
            format!("aimdb-{}", uuid::Uuid::new_v4())
        });

        let mut mqtt_opts = MqttOptions::new(
            client_id,
            config.url.host.clone(),
            config.url.effective_port().unwrap_or(1883),
        );

        mqtt_opts.set_keep_alive(Duration::from_secs(30));

        // Add credentials if provided
        if let (Some(ref username), Some(ref password)) = (&config.url.username, &config.url.password)
        {
            mqtt_opts.set_credentials(username, password);
        }

        // Create client and event loop
        let (client, event_loop) = AsyncClient::new(mqtt_opts, 10);

        // Spawn event loop task (required by rumqttc)
        spawn_event_loop(event_loop, broker_key.clone());

        let mqtt_client = Arc::new(MqttClient {
            client,
            broker_key: broker_key.clone(),
            is_secure: config.url.scheme() == "mqtts",
        });

        clients.insert(broker_key, mqtt_client.clone());
        mqtt_client
    }

    /// Get number of clients in the pool (for testing/debugging)
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Clear all clients from the pool (for testing)
    #[cfg(test)]
    pub fn clear(&self) {
        self.clients.lock().unwrap().clear();
    }
}

impl Default for MqttClientPool {
    fn default() -> Self {
        Self::new()
    }
}

// Implement the connector trait from aimdb-core
impl aimdb_core::pool::MqttConnectorPool for MqttClientPool {
    fn publish(
        &self,
        url: &str,
        config: &[(String, String)],
        bytes: Vec<u8>,
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<(), String>> + Send + '_>> {
        let url_owned = url.to_string();
        let config_owned: Vec<(String, String)> = config.to_vec();
        
        Box::pin(async move {
            let url = url_owned.as_str();
            // Parse the URL to get MQTT configuration
            let connector_url = aimdb_core::connector::ConnectorUrl::parse(url)
                .map_err(|e| format!("Invalid MQTT URL: {}", e))?;
            
            let mut mqtt_config = crate::MqttConfig::from_url(connector_url)
                .map_err(|e| format!("Invalid MQTT config: {}", e))?;
            
            // Apply additional config from key-value pairs
            for (key, value) in &config_owned {
                match key.as_str() {
                    "client_id" => mqtt_config = mqtt_config.with_client_id(value),
                    "qos" => {
                        if let Ok(qos_val) = value.parse::<u8>() {
                            mqtt_config = mqtt_config.with_qos(qos_val);
                        }
                    }
                    "retain" => {
                        if let Ok(retain_val) = value.parse::<bool>() {
                            mqtt_config = mqtt_config.with_retain(retain_val);
                        }
                    }
                    _ => {} // Ignore unknown config
                }
            }
            
            // Get or create client for this broker
            let mqtt_client = self.get_or_create_client(&mqtt_config);
            
            // Determine QoS
            let qos = match mqtt_config.qos {
                0 => rumqttc::QoS::AtMostOnce,
                1 => rumqttc::QoS::AtLeastOnce,
                2 => rumqttc::QoS::ExactlyOnce,
                _ => rumqttc::QoS::AtLeastOnce,
            };
            
            // Publish the message
            mqtt_client
                .publish(
                    mqtt_config.topic.clone(),
                    qos,
                    mqtt_config.retain,
                    bytes,
                )
                .await
                .map_err(|e| format!("MQTT publish failed: {}", e))?;
            
            Ok(())
        })
    }
}

/// MQTT client wrapper with metadata
///
/// Wraps the rumqttc AsyncClient along with connection information
/// for debugging and monitoring.
pub struct MqttClient {
    /// The underlying MQTT client
    pub client: AsyncClient,
    
    /// Broker identifier (host:port)
    #[allow(dead_code)]
    broker_key: String,
    
    /// Whether TLS is enabled
    #[allow(dead_code)]
    is_secure: bool,
}

impl MqttClient {
    /// Get the broker identifier (host:port)
    #[allow(dead_code)]
    pub fn broker_key(&self) -> &str {
        &self.broker_key
    }
    
    /// Check if TLS is enabled for this connection
    #[allow(dead_code)]
    pub fn is_secure(&self) -> bool {
        self.is_secure
    }
    
    /// Publish a message to a topic
    ///
    /// # Arguments
    /// * `topic` - MQTT topic to publish to
    /// * `qos` - Quality of Service level (0, 1, or 2)
    /// * `retain` - Whether message should be retained by broker
    /// * `payload` - Message payload as bytes
    ///
    /// # Returns
    /// * `Ok(())` on successful publish (queued, not confirmed)
    /// * `Err(_)` if client is disconnected or queue is full
    pub async fn publish(
        &self,
        topic: impl Into<String>,
        qos: QoS,
        retain: bool,
        payload: impl Into<Vec<u8>>,
    ) -> Result<(), rumqttc::ClientError> {
        self.client.publish(topic, qos, retain, payload).await
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
/// * `broker_key` - Broker identifier for logging
fn spawn_event_loop(mut event_loop: EventLoop, broker_key: String) {
    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::debug!("MQTT event loop started for {}", broker_key);

        loop {
            match event_loop.poll().await {
                Ok(_notification) => {
                    // Event loop is running normally
                    // Notifications include: Incoming publishes, connection status, etc.
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("MQTT event loop error for {}: {:?}", broker_key, e);

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
    async fn test_pool_singleton_per_broker() {
        let pool = MqttClientPool::new();

        let url = ConnectorUrl::parse("mqtt://broker1:1883/topic").unwrap();
        let config1 = MqttConfig::from_url(url.clone()).unwrap();
        let config2 = MqttConfig::from_url(url).unwrap();

        let client1 = pool.get_or_create_client(&config1);
        let client2 = pool.get_or_create_client(&config2);

        // Should be the same Arc (pointer equality)
        assert!(Arc::ptr_eq(&client1, &client2), "Clients should be the same instance");
        assert_eq!(pool.client_count(), 1);
    }

    #[tokio::test]
    async fn test_pool_different_brokers() {
        let pool = MqttClientPool::new();

        let url1 = ConnectorUrl::parse("mqtt://broker1:1883/topic").unwrap();
        let url2 = ConnectorUrl::parse("mqtt://broker2:1883/topic").unwrap();

        let config1 = MqttConfig::from_url(url1).unwrap();
        let config2 = MqttConfig::from_url(url2).unwrap();

        let client1 = pool.get_or_create_client(&config1);
        let client2 = pool.get_or_create_client(&config2);

        // Should be different instances
        assert!(!Arc::ptr_eq(&client1, &client2), "Different brokers should have different clients");
        assert_ne!(client1.broker_key, client2.broker_key);
        assert_eq!(pool.client_count(), 2);
    }

    #[tokio::test]
    async fn test_add_broker_by_url() {
        let pool = MqttClientPool::new();

        pool.add_broker("mqtt://broker1:1883/topic").await.unwrap();
        pool.add_broker("mqtt://broker2:1883/topic").await.unwrap();
        
        assert_eq!(pool.client_count(), 2);
        
        // Adding same broker again should not increase count
        pool.add_broker("mqtt://broker1:1883/topic").await.unwrap();
        assert_eq!(pool.client_count(), 2);
    }

    #[tokio::test]
    async fn test_broker_key_includes_port() {
        let pool = MqttClientPool::new();

        let url = ConnectorUrl::parse("mqtt://broker:9999/topic").unwrap();
        let config = MqttConfig::from_url(url).unwrap();

        let client = pool.get_or_create_client(&config);

        assert_eq!(client.broker_key, "broker:9999");
    }

    #[tokio::test]
    async fn test_default_port_in_broker_key() {
        let pool = MqttClientPool::new();

        let url = ConnectorUrl::parse("mqtt://broker/topic").unwrap();
        let config = MqttConfig::from_url(url).unwrap();

        let client = pool.get_or_create_client(&config);

        assert_eq!(client.broker_key, "broker:1883");
    }

    #[tokio::test]
    async fn test_secure_flag() {
        let pool = MqttClientPool::new();

        let url_mqtt = ConnectorUrl::parse("mqtt://broker/topic").unwrap();
        let url_mqtts = ConnectorUrl::parse("mqtts://broker/topic").unwrap();

        let config_mqtt = MqttConfig::from_url(url_mqtt).unwrap();
        let config_mqtts = MqttConfig::from_url(url_mqtts).unwrap();

        let client_mqtt = pool.get_or_create_client(&config_mqtt);
        let client_mqtts = pool.get_or_create_client(&config_mqtts);

        assert!(!client_mqtt.is_secure);
        assert!(client_mqtts.is_secure);
    }

    #[tokio::test]
    async fn test_pool_clear() {
        let pool = MqttClientPool::new();

        pool.add_broker("mqtt://broker1:1883/topic").await.unwrap();
        pool.add_broker("mqtt://broker2:1883/topic").await.unwrap();
        assert_eq!(pool.client_count(), 2);

        pool.clear();
        assert_eq!(pool.client_count(), 0);
    }
}
