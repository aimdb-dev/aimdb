//! MQTT client management and lifecycle
//!
//! This module provides a client pool that:
//! - Manages a single MQTT broker connection
//! - Automatic event loop spawning
//! - Thread-safe access from multiple consumers
//! - Explicit lifecycle management (user controls when clients are created)

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{pump_sink, pump_source, BoxFut, ConnectorBuilder, Payload, Source};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet};
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
///            .with_remote_access();
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

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

impl<R: aimdb_executor::RuntimeAdapter + 'static> ConnectorBuilder<R> for MqttConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async move {
            // Build a router from the inbound routes purely to drive the MQTT
            // subscriptions + channel-capacity sizing in `build_internal`. The
            // routing `Router` that fans incoming frames out to producers is
            // (re)built by `pump_source` from the same `collect_inbound_routes`.
            let inbound_routes = db.collect_inbound_routes("mqtt");
            let router = RouterBuilder::from_routes(inbound_routes).build();

            #[cfg(feature = "tracing")]
            tracing::info!("MQTT subscribing to {} topics", router.resource_ids().len());

            // Connect, subscribe, and hand back the raw event loop.
            let (client, event_loop) =
                MqttConnectorImpl::build_internal(&self.broker_url, self.client_id.clone(), router)
                    .await
                    .map_err(|_e| {
                        #[cfg(feature = "std")]
                        {
                            aimdb_core::DbError::RuntimeError {
                                message: format!("Failed to build MQTT connector: {}", _e),
                            }
                        }
                        #[cfg(not(feature = "std"))]
                        {
                            aimdb_core::DbError::RuntimeError { _message: () }
                        }
                    })?;

            let mut futures: Vec<BoxFuture> = Vec::new();

            // Inbound: one multiplexed reader future fanning publishes out to producers.
            futures.extend(pump_source(
                db,
                "mqtt",
                MqttEventLoopSource {
                    event_loop,
                    #[cfg(feature = "tracing")]
                    broker_key: self.broker_url.clone(),
                },
            ));

            // Outbound: one publisher future per outbound route.
            futures.extend(pump_sink(db, "mqtt", Arc::new(MqttSink { client })));

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Internal MQTT connector build helpers.
///
/// A namespace for the broker-connection setup invoked from
/// [`MqttConnectorBuilder::build`]; the data-plane loops themselves live in the
/// reusable `pump_sink` / `pump_source` helpers + the [`MqttSink`] /
/// [`MqttEventLoopSource`] adapters below.
pub struct MqttConnectorImpl;

impl MqttConnectorImpl {
    /// Connect to the broker and subscribe to all configured topics (internal).
    ///
    /// Creates the MQTT client, sizes the send-channel from the route count, and
    /// subscribes to every topic in `router`. Returns the shared client (for the
    /// outbound `pump_sink`) plus the raw event loop (handed to a
    /// [`MqttEventLoopSource`] for the inbound `pump_source`).
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    /// * `client_id` - Optional client ID (if None, generates UUID-based ID)
    /// * `router` - Routes used only for the subscription list + capacity sizing
    async fn build_internal(
        broker_url: &str,
        client_id: Option<String>,
        router: Router,
    ) -> Result<(Arc<AsyncClient>, EventLoop), String> {
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

        #[cfg(feature = "tracing")]
        tracing::info!("Creating MQTT client for {}:{}", host, port);

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

        // Wrap router early so we can count topics for capacity calculation
        let router_arc = Arc::new(router);
        let topic_count = router_arc.resource_ids().len();

        // Dynamic channel capacity: scales with topic count.
        //
        // With spawn-before-subscribe, the event loop drains continuously, so the
        // client send buffer only needs a small fixed headroom to absorb short
        // bursts of publishes and QoS handshake packets (PUBACK/PUBREC/PUBREL/PUBCOMP).
        //
        // A value of 10 has been chosen empirically as a conservative upper bound
        // for typical burst sizes in this connector without over-allocating, while
        // still keeping backpressure behavior predictable.
        const CHANNEL_HEADROOM: usize = 10;
        let channel_capacity = topic_count + CHANNEL_HEADROOM;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "MQTT channel capacity set to {} (for {} topics)",
            channel_capacity,
            topic_count
        );

        // Create client and event loop with dynamic capacity
        let (client, event_loop) = AsyncClient::new(mqtt_opts, channel_capacity);
        let client_arc = Arc::new(client);

        let topics = router_arc.resource_ids();

        #[cfg(feature = "tracing")]
        tracing::info!("Subscribing to {} MQTT topics...", topics.len());

        for topic in &topics {
            #[cfg(feature = "tracing")]
            tracing::debug!("Subscribing to MQTT topic: {}", topic);

            client_arc
                .subscribe(topic.as_ref(), rumqttc::QoS::AtLeastOnce)
                .await
                .map_err(|e| format!("Failed to subscribe to topic '{}': {}", topic, e))?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!("MQTT subscriptions complete");

        Ok((client_arc, event_loop))
    }
}

/// Pure outbound publish adapter driven by `pump_sink`.
///
/// Wraps the shared rumqttc client. `qos`/`retain` come from the route's protocol
/// options (threaded through by `pump_sink` via [`ConnectorConfig::from_query`]),
/// interpreted with MQTT's legacy defaults — **QoS 1 (`AtLeastOnce`)** when
/// unspecified, no retain — so the wire stays byte-identical to the old loop.
struct MqttSink {
    client: Arc<AsyncClient>,
}

impl MqttSink {
    /// Look up a protocol option by key and parse it.
    fn opt<T: core::str::FromStr>(config: &ConnectorConfig, key: &str) -> Option<T> {
        config
            .protocol_options
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.parse().ok())
    }
}

impl Connector for MqttSink {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Legacy defaults: QoS 1 when no `qos` query option, no retain.
        let qos = Self::opt::<u8>(config, "qos").unwrap_or(1);
        let retain = Self::opt::<bool>(config, "retain").unwrap_or(false);

        // Destination is already the MQTT topic (from ConnectorUrl::resource_id()).
        let topic = destination.to_string();
        let payload_owned = payload.to_vec();
        let client = self.client.clone();

        Box::pin(async move {
            let qos_level = match qos {
                0 => rumqttc::QoS::AtMostOnce,
                1 => rumqttc::QoS::AtLeastOnce,
                2 => rumqttc::QoS::ExactlyOnce,
                _ => return Err(PublishError::UnsupportedQoS),
            };

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

/// Inbound frame source driven by `pump_source`.
///
/// Yields `(topic, payload)` for each incoming MQTT publish. The inner poll loop
/// discards non-publish packets — keeping QoS handshakes and keepalive flowing —
/// and backs off 5s on a connection error before retrying, reproducing the old
/// hand-rolled event-loop future exactly. It never yields `None`: the reader runs
/// for the lifetime of the connector.
struct MqttEventLoopSource {
    event_loop: EventLoop,
    #[cfg(feature = "tracing")]
    broker_key: String,
}

impl Source for MqttEventLoopSource {
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move {
            loop {
                match self.event_loop.poll().await {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        let topic = publish.topic.clone();
                        let payload: Payload = Arc::from(publish.payload.as_ref());

                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "Received MQTT message on topic '{}' ({} bytes)",
                            topic,
                            payload.len()
                        );

                        return Some((topic, payload));
                    }
                    // Non-publish packets (PUBACK/PINGRESP/…) keep driving the protocol.
                    Ok(_) => continue,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!("MQTT event loop error for {}: {:?}", self.broker_key, _e);

                        // Wait before reconnecting.
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        })
    }
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
