//! MQTT client management and lifecycle
//!
//! This module provides a client pool that:
//! - Manages a single MQTT broker connection
//! - Automatic event loop spawning
//! - Thread-safe access from multiple consumers
//! - Explicit lifecycle management (user controls when clients are created)

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use rumqttc::{AsyncClient, EventLoop, MqttOptions, Packet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// MQTT connector for a single broker connection with router-based dispatch
///
/// Each connector manages ONE MQTT broker connection. The router determines
/// how incoming messages are dispatched to AimDB producers.
///
/// # Usage Pattern
///
/// ```rust,ignore
/// use aimdb_mqtt_connector::MqttConnector;
///
/// // Configure database with MQTT links
/// let db = AimDbBuilder::new()
///     .runtime(runtime)
///     .with_connector(MqttConnector::new("mqtt://localhost:1883"))
///     .configure::<Temperature>(|reg| {
///         reg.link_from("mqtt://commands/temp")
///            .with_deserializer(deserialize_temp)
///            .with_buffer(BufferCfg::SingleLatest)
///            .with_serialization();
///     })
///     .build().await?;
/// ```
///
/// The connector collects routes from the database during build() and
/// automatically subscribes to all required MQTT topics.
pub struct MqttConnectorBuilder {
    broker_url: String,
    client_id: Option<String>,
}

impl MqttConnectorBuilder {
    /// Create a new MQTT connector builder
    ///
    /// If no client ID is explicitly set via `with_client_id()`, a random
    /// UUID-based client ID will be generated automatically when the connector
    /// is built.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = MqttConnector::new("mqtt://localhost:1883");
    /// ```
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: None,
        }
    }

    /// Set the MQTT client ID
    ///
    /// The client ID should be unique for each client connecting to the broker.
    /// It's used for session persistence and message delivery guarantees.
    ///
    /// If not set, a random UUID-based client ID will be generated automatically.
    ///
    /// # Arguments
    /// * `client_id` - Unique identifier for this client
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = MqttConnector::new("mqtt://localhost:1883")
    ///     .with_client_id("my-app-001");
    /// ```
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }
}

impl<R: aimdb_executor::Spawn + 'static> ConnectorBuilder<R> for MqttConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<
        Box<
            dyn Future<Output = aimdb_core::DbResult<Arc<dyn aimdb_core::transport::Connector>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            // Collect inbound routes from database
            let inbound_routes = db.collect_inbound_routes("mqtt");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "Collected {} inbound routes for MQTT connector",
                inbound_routes.len()
            );

            // Convert routes to Router
            let router = RouterBuilder::from_routes(inbound_routes).build();

            #[cfg(feature = "tracing")]
            tracing::info!("MQTT router has {} topics", router.resource_ids().len());

            // Build the actual connector
            let connector =
                MqttConnectorImpl::build_internal(&self.broker_url, self.client_id.clone(), router)
                    .await
                    .map_err(|e| {
                        #[cfg(feature = "std")]
                        {
                            aimdb_core::DbError::RuntimeError {
                                message: format!("Failed to build MQTT connector: {}", e).into(),
                            }
                        }
                        #[cfg(not(feature = "std"))]
                        {
                            aimdb_core::DbError::RuntimeError { _message: () }
                        }
                    })?;

            // NEW: Collect and spawn outbound publishers
            let outbound_routes = db.collect_outbound_routes("mqtt");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "Collected {} outbound routes for MQTT connector",
                outbound_routes.len()
            );

            connector.spawn_outbound_publishers(db, outbound_routes)?;

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        })
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Internal MQTT connector implementation
///
/// This is the actual connector created after collecting routes from the database.
pub struct MqttConnectorImpl {
    client: Arc<AsyncClient>,
    router: Arc<Router>,
}

impl MqttConnectorImpl {
    /// Create a new MQTT connector with pre-configured router (internal)
    ///
    /// Creates a connection to the MQTT broker and subscribes to all topics
    /// defined in the router. The event loop is spawned automatically.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    /// * `client_id` - Optional client ID (if None, generates UUID-based ID)
    /// * `router` - Pre-configured router with all routes
    async fn build_internal(
        broker_url: &str,
        client_id: Option<String>,
        router: Router,
    ) -> Result<Self, String> {
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

        // Use provided client_id or generate a UUID-based one
        let client_id = client_id.unwrap_or_else(|| format!("aimdb-{}", uuid::Uuid::new_v4()));

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

        let router_arc = Arc::new(router);
        let client_arc = Arc::new(client);

        // Subscribe to topics from the router
        let topics = router_arc.resource_ids();
        for topic in &topics {
            #[cfg(feature = "tracing")]
            tracing::debug!("Subscribing to MQTT topic: {}", topic);

            client_arc
                .subscribe(topic.as_ref(), rumqttc::QoS::AtLeastOnce)
                .await
                .map_err(|e| format!("Failed to subscribe to topic '{}': {}", topic, e))?;
        }

        // Spawn event loop task with router
        spawn_event_loop(event_loop, broker_key, router_arc.clone());

        Ok(Self {
            client: client_arc,
            router: router_arc,
        })
    }

    /// Get list of all MQTT topics this connector is subscribed to
    ///
    /// Returns the unique topics from the router configuration.
    /// Useful for debugging and monitoring.
    pub fn topics(&self) -> Vec<Arc<str>> {
        self.router.resource_ids()
    }

    /// Get the number of routes configured in this connector
    ///
    /// Each route represents a (topic, type) mapping.
    /// Multiple routes can exist for the same topic if different types subscribe to it.
    pub fn route_count(&self) -> usize {
        self.router.route_count()
    }

    /// Spawns outbound publisher tasks for all configured routes (internal)
    ///
    /// Called automatically during build() to start publishing data from AimDB to MQTT.
    /// Each route spawns an independent task that subscribes to the record
    /// and publishes to the MQTT broker.
    ///
    /// This method uses the ConsumerTrait + factory pattern to subscribe to
    /// typed records without knowing the concrete type T at compile time.
    fn spawn_outbound_publishers<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        routes: Vec<(
            String,
            Box<dyn aimdb_core::connector::ConsumerTrait>,
            aimdb_core::connector::SerializerFn,
            Vec<(String, String)>,
        )>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();

        for (topic, consumer, serializer, config) in routes {
            let client = self.client.clone();
            let topic_clone = topic.clone();

            // Parse config options
            let mut qos = rumqttc::QoS::AtLeastOnce; // Default
            let mut retain = false;

            for (key, value) in &config {
                match key.as_str() {
                    "qos" => {
                        if let Ok(qos_val) = value.parse::<u8>() {
                            qos = match qos_val {
                                0 => rumqttc::QoS::AtMostOnce,
                                1 => rumqttc::QoS::AtLeastOnce,
                                2 => rumqttc::QoS::ExactlyOnce,
                                _ => rumqttc::QoS::AtLeastOnce,
                            };
                        }
                    }
                    "retain" => {
                        if let Ok(retain_val) = value.parse::<bool>() {
                            retain = retain_val;
                        }
                    }
                    _ => {}
                }
            }

            runtime.spawn(async move {
                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Failed to subscribe for outbound topic '{}': {:?}",
                            topic_clone,
                            _e
                        );
                        return;
                    }
                };

                #[cfg(feature = "tracing")]
                tracing::info!("MQTT outbound publisher started for topic: {}", topic_clone);

                while let Ok(value_any) = reader.recv_any().await {
                    // Serialize the type-erased value
                    let bytes = match serializer(&*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "Failed to serialize for topic '{}': {:?}",
                                topic_clone,
                                _e
                            );
                            continue;
                        }
                    };

                    // Publish to MQTT with protocol-specific config
                    if let Err(_e) = client.publish(&topic_clone, qos, retain, bytes).await {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Failed to publish to MQTT topic '{}': {:?}",
                            topic_clone,
                            _e
                        );
                    } else {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("Published to MQTT topic: {}", topic_clone);
                    }
                }

                #[cfg(feature = "tracing")]
                tracing::info!("MQTT outbound publisher stopped for topic: {}", topic_clone);
            })?;
        }

        Ok(())
    }
}

// Implement the connector trait from aimdb-core
impl aimdb_core::transport::Connector for MqttConnectorImpl {
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

        // Destination is already the MQTT topic (from ConnectorUrl::resource_id())
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

    // Note: subscribe() method removed in v0.2.0
    // Inbound routing now uses the MqttRouter passed to new()
}

/// Spawn the MQTT event loop in a background task with router-based dispatch
///
/// The event loop is required by rumqttc to handle:
/// - Network I/O (reading/writing packets)
/// - Reconnection logic
/// - QoS handshakes
/// - Routing incoming publishes to AimDB producers
///
/// # Arguments
/// * `event_loop` - The rumqttc EventLoop to run
/// * `_broker_key` - Broker identifier for logging (unused in release builds)
/// * `router` - Router for dispatching messages to producers
fn spawn_event_loop(mut event_loop: EventLoop, _broker_key: String, router: Arc<Router>) {
    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::debug!("MQTT event loop started for {}", _broker_key);

        loop {
            match event_loop.poll().await {
                Ok(notification) => {
                    // Route incoming publishes via the router
                    if let rumqttc::Event::Incoming(Packet::Publish(publish)) = notification {
                        let topic = publish.topic.clone();
                        let payload = publish.payload.to_vec();

                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "Received MQTT message on topic '{}' ({} bytes)",
                            topic,
                            payload.len()
                        );

                        // Route to appropriate producer(s)
                        if let Err(_e) = router.route(&topic, &payload).await {
                            #[cfg(feature = "tracing")]
                            tracing::error!("Failed to route message on topic '{}': {}", topic, _e);
                        }
                    }
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
    use aimdb_core::router::RouterBuilder;

    #[tokio::test]
    async fn test_connector_creation_with_router() {
        let router = RouterBuilder::new().build();
        let connector =
            MqttConnectorImpl::build_internal("mqtt://localhost:1883", None, router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let router = RouterBuilder::new().build();
        let connector =
            MqttConnectorImpl::build_internal("mqtt://broker.local:9999", None, router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let router = RouterBuilder::new().build();
        let connector = MqttConnectorImpl::build_internal("not-a-valid-url", None, router).await;
        assert!(connector.is_err());
    }
}
