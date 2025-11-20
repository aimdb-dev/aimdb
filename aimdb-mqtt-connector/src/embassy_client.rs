//! Embassy MQTT client implementation using mountain-mqtt-embassy
//!
//! This module provides production-ready MQTT connectivity for Embassy-based
//! embedded systems using mountain-mqtt-embassy's `run()` function.
//!
//! # Architecture
//!
//! Uses the ConnectorBuilder pattern integrated with AimDB:
//! - `MqttConnectorBuilder` is used with `.with_connector()` during database setup
//! - Background tasks are spawned automatically during connector initialization
//! - MQTT manager handles connection, reconnection, and message delivery
//! - Event router forwards incoming MQTT messages to appropriate record producers
//!
//! # Usage
//!
//! ```rust,ignore
//! use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
//! use aimdb_core::AimDbBuilder;
//!
//! // Configure database with MQTT connector
//! let db = AimDbBuilder::new()
//!     .runtime(embassy_adapter)
//!     .with_connector(
//!         MqttConnectorBuilder::new("mqtt://192.168.1.100:1883")
//!             .with_client_id("my-unique-device-id")
//!     )
//!     .configure::<Temperature>(|reg| {
//!         // Outbound: Publish temperature readings to MQTT
//!         reg.link_to("mqtt://sensors/temperature")
//!            .with_serializer(|temp| Ok(temp.to_json()))
//!            .finish();
//!         
//!         // Inbound: Subscribe to commands from MQTT
//!         reg.link_from("mqtt://commands/temperature")
//!            .with_deserializer(|data| TemperatureCommand::from_json(data))
//!            .finish();
//!     })
//!     .build().await?;
//! ```

extern crate alloc;

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::transport::PublishError;
use aimdb_core::ConnectorBuilder;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::net::Ipv4Addr;
use core::pin::Pin;
use core::str::FromStr;

use embassy_net::Ipv4Address;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_sync::once_lock::OnceLock;
use static_cell::StaticCell;

/// Type alias for outbound route configuration
/// (resource_id, consumer, serializer, config_params)
type OutboundRoute = (
    String,
    Box<dyn aimdb_core::connector::ConsumerTrait>,
    aimdb_core::connector::SerializerFn,
    Vec<(String, String)>,
);

use mountain_mqtt::client::{Client, ClientError, ConnectionSettings};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::mqtt_manager::{ConnectionId, MqttOperations};
use mountain_mqtt_embassy::mqtt_manager::{self, MqttEvent, Settings};

/// Maximum number of pending MQTT actions and events
const CHANNEL_SIZE: usize = 32;

/// Buffer size for MQTT packets (4KB)
const BUFFER_SIZE: usize = 4096;

/// Maximum properties in MQTT packets
const MAX_PROPERTIES: usize = 16;

/// MQTT actions that can be performed
///
/// Implements the `MqttOperations` trait required by mountain-mqtt-embassy.
#[derive(Clone)]
pub enum AimdbMqttAction {
    /// Publish a message to a topic
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: QualityOfService,
        retain: bool,
    },
    /// Subscribe to a topic
    Subscribe {
        topic: String,
        qos: QualityOfService,
    },
}

/// Implementation of MqttOperations trait for AimDB actions
impl MqttOperations for AimdbMqttAction {
    async fn perform<'a, 'b, C>(
        &'b mut self,
        client: &mut C,
        _client_id: &'a str,
        _connection_id: ConnectionId,
        _is_retry: bool,
    ) -> Result<(), ClientError>
    where
        C: Client<'a>,
    {
        match self {
            Self::Publish {
                topic,
                payload,
                qos,
                retain,
            } => {
                #[cfg(feature = "defmt")]
                {
                    if is_retry {
                        defmt::debug!("Retrying publish to {}", topic);
                    } else {
                        defmt::debug!(
                            "Publishing {} bytes to {} (QoS={:?})",
                            payload.len(),
                            topic,
                            qos
                        );
                    }
                }

                client.publish(topic, payload, *qos, *retain).await?;

                #[cfg(feature = "defmt")]
                defmt::info!("Published {} bytes to {}", payload.len(), topic);

                Ok(())
            }
            Self::Subscribe { topic, qos } => {
                #[cfg(feature = "defmt")]
                {
                    if is_retry {
                        defmt::debug!("Retrying subscribe to {} (QoS={:?})", topic, qos);
                    } else {
                        defmt::info!("Subscribing to {} (QoS={:?})", topic, qos);
                    }
                }

                client.subscribe(topic, *qos).await?;

                #[cfg(feature = "defmt")]
                defmt::info!("Subscribed to {}", topic);

                Ok(())
            }
        }
    }
}

/// MQTT events for received messages
///
/// Handles incoming MQTT messages that will be routed to the appropriate
/// record producers via the router.
#[derive(Clone)]
pub enum AimdbMqttEvent {
    /// A message was received from a subscribed topic
    MessageReceived {
        /// The topic the message was received on
        topic: String,
        /// The message payload
        payload: Vec<u8>,
    },
}

impl mountain_mqtt_embassy::mqtt_manager::FromApplicationMessage<MAX_PROPERTIES>
    for AimdbMqttEvent
{
    fn from_application_message(
        message: &mountain_mqtt::packets::publish::ApplicationMessage<MAX_PROPERTIES>,
    ) -> Result<Self, mountain_mqtt::client::EventHandlerError> {
        #[cfg(feature = "defmt")]
        defmt::debug!(
            "Received message on topic '{}', {} bytes",
            message.topic_name,
            message.payload.len()
        );

        Ok(Self::MessageReceived {
            topic: message.topic_name.to_string(),
            payload: message.payload.to_vec(),
        })
    }
}

/// MQTT connector builder for Embassy with router-based dispatch
///
/// Similar to the Tokio version, this builder creates an MQTT connector
/// that integrates with AimDB's routing system. The builder collects routes
/// from the database during build() and automatically subscribes to topics.
///
/// # Usage Pattern
///
/// ```rust,ignore
/// use aimdb_mqtt_connector::embassy::MqttConnectorBuilder;
///
/// // Configure database with MQTT links
/// let db = AimDbBuilder::new()
///     .runtime(embassy_adapter)
///     .with_connector(
///         MqttConnectorBuilder::new("mqtt://192.168.1.100:1883")
///             .with_client_id("my-device-001")
///     )
///     .configure::<Temperature>(|reg| {
///         reg.link_from("mqtt://commands/temp")
///            .with_deserializer(deserialize_temp)
///            .with_buffer(BufferCfg::SingleLatest);
///     })
///     .build().await?;
/// ```
pub struct MqttConnectorBuilder {
    broker_url: String,
    client_id: String,
}

impl MqttConnectorBuilder {
    /// Create a new MQTT connector builder for Embassy
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL in format `mqtt://host:port`
    ///   - Scheme: `mqtt://` (plain TCP)
    ///   - Host: IP address or hostname (e.g., "192.168.1.100")
    ///   - Port: MQTT broker port (default: 1883)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = MqttConnectorBuilder::new("mqtt://192.168.1.100:1883")
    ///     .with_client_id("sensor-node-42");
    /// ```
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: "aimdb-client".to_string(),
        }
    }

    /// Set the MQTT client ID
    ///
    /// The client ID should be unique for each device connecting to the broker.
    /// It's used for session persistence and message delivery guarantees.
    ///
    /// # Arguments
    /// * `client_id` - Unique identifier for this client (max 32 chars recommended)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = MqttConnectorBuilder::new("mqtt://192.168.1.100:1883")
    ///     .with_client_id("my-unique-device-id");
    /// ```
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = client_id.into();
        self
    }
}

/// Implement ConnectorBuilder trait for Embassy runtime with network stack access
///
/// This implementation requires the runtime to provide the `EmbassyNetwork` trait
/// so the connector can access the network stack for creating TCP connections.
impl<R> ConnectorBuilder<R> for MqttConnectorBuilder
where
    R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork + 'static,
{
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
        // Wrap in SendFutureWrapper since Embassy types aren't Send but we're single-threaded
        Box::pin(SendFutureWrapper(async move {
            // Collect inbound routes from database
            let routes = db.collect_inbound_routes("mqtt");

            #[cfg(feature = "defmt")]
            defmt::info!(
                "Collected {} inbound routes for MQTT connector",
                routes.len()
            );

            // Convert routes to Router
            let router = RouterBuilder::from_routes(routes).build();

            #[cfg(feature = "defmt")]
            defmt::info!("MQTT router has {} topics", router.resource_ids().len());

            // Build the actual connector
            let connector = MqttConnectorImpl::build_internal(
                &self.broker_url,
                &self.client_id,
                router,
                db.runtime(),
            )
            .await
            .map_err(|_e| {
                #[cfg(feature = "defmt")]
                defmt::error!("Failed to build MQTT connector");

                aimdb_core::DbError::RuntimeError { _message: () }
            })?;

            // Collect and spawn outbound publishers
            let outbound_routes = db.collect_outbound_routes("mqtt");

            #[cfg(feature = "defmt")]
            defmt::info!(
                "Collected {} outbound routes for MQTT connector",
                outbound_routes.len()
            );

            connector.spawn_outbound_publishers(db, outbound_routes)?;

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        }))
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Internal MQTT connector implementation for Embassy
///
/// This is the actual connector created after collecting routes from the database.
/// It manages the MQTT connection and routing of incoming messages to producers.
pub struct MqttConnectorImpl {
    #[allow(dead_code)] // Stored for future use (stats, topic lists, etc.)
    router: Arc<Router>,
    action_sender: Sender<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
}

// SAFETY: MqttConnectorImpl is safe to use in Embassy's single-threaded environment.
// The Sender is not Sync, but Embassy tasks run on a single thread.
unsafe impl Send for MqttConnectorImpl {}
unsafe impl Sync for MqttConnectorImpl {}

impl MqttConnectorImpl {
    /// Create a new MQTT connector with pre-configured router (internal)
    ///
    /// Creates a connection to the MQTT broker and subscribes to all topics
    /// defined in the router. The background task is spawned automatically.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port)
    /// * `client_id` - MQTT client identifier
    /// * `router` - Pre-configured router with all routes
    /// * `runtime` - Embassy runtime adapter for spawning and network access
    async fn build_internal<R>(
        broker_url: &str,
        client_id: &str,
        router: Router,
        runtime: &R,
    ) -> Result<Self, &'static str>
    where
        R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork + 'static,
    {
        // Parse the broker URL
        let mut url = broker_url.to_string();

        // If no topic is provided, add a dummy one for parsing
        if !url.contains('/') || url.matches('/').count() < 3 {
            url = format!("{}/dummy", url.trim_end_matches('/'));
        }

        let connector_url = ConnectorUrl::parse(&url).map_err(|_| "Invalid MQTT URL")?;

        let host = connector_url.host.clone();
        let port = connector_url.port.unwrap_or(1883);

        #[cfg(feature = "defmt")]
        defmt::info!("Creating MQTT connector for {}:{}", host, port);

        // Parse broker IP address
        let broker_ip = Ipv4Addr::from_str(&host).map_err(|_| "Invalid broker IP address")?;

        // Convert to embassy Ipv4Address
        let octets = broker_ip.octets();
        let broker_addr = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

        // Store client_id in static memory for 'static lifetime requirement
        // Uses OnceLock to gracefully handle multiple initialization attempts
        static CLIENT_ID_STORAGE: OnceLock<String> = OnceLock::new();
        let client_id_static: &'static str =
            CLIENT_ID_STORAGE.get_or_init(|| client_id.to_string());

        #[cfg(feature = "defmt")]
        defmt::info!("MQTT client ID: {}", client_id_static);

        // Create static channels for MQTT communication
        static ACTION_CHANNEL: StaticCell<Channel<NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>> =
            StaticCell::new();
        static EVENT_CHANNEL: StaticCell<
            Channel<NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
        > = StaticCell::new();

        // Try to initialize channels (will panic if called twice - expected in embedded)
        let action_channel = ACTION_CHANNEL.init(Channel::new());
        let event_channel = EVENT_CHANNEL.init(Channel::new());

        let action_sender = action_channel.sender();
        let action_receiver = action_channel.receiver();
        let event_sender = event_channel.sender();
        let event_receiver = event_channel.receiver();

        // Create connection settings
        let connection_settings = ConnectionSettings::unauthenticated(client_id_static);

        // Create mqtt_manager settings
        let settings = Settings::new(broker_addr, port);

        // Clone router for background task
        let router_arc = Arc::new(router);
        let router_for_task = router_arc.clone();

        // Get topics to subscribe to from router
        let topics = router_arc.resource_ids();

        #[cfg(feature = "defmt")]
        defmt::info!("Will subscribe to {} MQTT topics", topics.len());

        // Get network stack for background task
        let network = runtime.network_stack();

        // Spawn MQTT manager background task
        let mqtt_task_future = SendFutureWrapper(async move {
            #[cfg(feature = "defmt")]
            defmt::info!("MQTT background task starting");

            // Run the MQTT manager (this never returns)
            #[allow(unreachable_code)]
            {
                let _: () = mqtt_manager::run::<
                    AimdbMqttAction,
                    AimdbMqttEvent,
                    MAX_PROPERTIES,
                    BUFFER_SIZE,
                    CHANNEL_SIZE,
                >(
                    *network, // Dereference the network stack reference
                    connection_settings,
                    settings,
                    event_sender,
                    action_receiver,
                )
                .await;
            }
        });

        // Spawn the task
        runtime
            .spawn(mqtt_task_future)
            .map_err(|_| "Failed to spawn MQTT manager task")?;

        #[cfg(feature = "defmt")]
        defmt::info!("MQTT manager task spawned successfully");

        // Spawn event monitoring task for inbound message routing
        let event_router_future = SendFutureWrapper(async move {
            #[cfg(feature = "defmt")]
            defmt::info!("MQTT event router task starting");

            loop {
                let event = event_receiver.receive().await;

                match event {
                    MqttEvent::ApplicationEvent {
                        event: AimdbMqttEvent::MessageReceived { topic, payload },
                        ..
                    } => {
                        #[cfg(feature = "defmt")]
                        defmt::debug!(
                            "Routing MQTT message from topic '{}', {} bytes",
                            topic,
                            payload.len()
                        );

                        // Route the message through the router to the appropriate producer
                        if let Err(_e) = router_for_task.route(&topic, &payload).await {
                            #[cfg(feature = "defmt")]
                            defmt::warn!("Failed to route MQTT message from '{}': {:?}", topic, _e);
                        }
                    }
                    _ => {
                        // Ignore other events (Connected, Disconnected, etc.)
                        #[cfg(feature = "defmt")]
                        defmt::trace!("Ignoring MQTT event (not a message)");
                    }
                }
            }
        });

        // Spawn the event router task
        runtime
            .spawn(event_router_future)
            .map_err(|_| "Failed to spawn MQTT event router task")?;

        #[cfg(feature = "defmt")]
        defmt::info!("MQTT event router task spawned successfully");

        // Subscribe to all topics from the router
        for topic in &topics {
            #[cfg(feature = "defmt")]
            defmt::info!("Subscribing to MQTT topic: {}", topic);

            let subscribe_action = AimdbMqttAction::Subscribe {
                topic: topic.to_string(),
                qos: QualityOfService::Qos1, // Use QoS 1 for reliable delivery
            };

            action_sender.send(subscribe_action).await;
        }

        #[cfg(feature = "defmt")]
        defmt::info!("Queued subscriptions for {} topics", topics.len());

        // Return the connector with action sender
        Ok(Self {
            router: router_arc,
            action_sender,
        })
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
        routes: Vec<OutboundRoute>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();

        for (topic, consumer, serializer, config) in routes {
            let action_sender = self.action_sender;
            let topic_clone = topic.clone();

            // Parse config options
            let mut qos = QualityOfService::Qos1; // Default
            let mut retain = false;

            for (key, value) in &config {
                match key.as_str() {
                    "qos" => {
                        if let Ok(qos_val) = value.parse::<u8>() {
                            qos = Self::map_qos(qos_val);
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

            runtime.spawn(SendFutureWrapper(async move {
                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "defmt")]
                        defmt::error!("Failed to subscribe for outbound topic '{}'", topic_clone);
                        return;
                    }
                };

                #[cfg(feature = "defmt")]
                defmt::info!("MQTT outbound publisher started for topic: {}", topic_clone);

                while let Ok(value_any) = reader.recv_any().await {
                    // Serialize the type-erased value
                    let bytes = match serializer(&*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "defmt")]
                            defmt::error!(
                                "Failed to serialize for topic '{}': {:?}",
                                topic_clone,
                                _e
                            );
                            continue;
                        }
                    };

                    // Publish to MQTT via action channel
                    let action = AimdbMqttAction::Publish {
                        topic: topic_clone.clone(),
                        payload: bytes,
                        qos,
                        retain,
                    };

                    action_sender.send(action).await;

                    #[cfg(feature = "defmt")]
                    defmt::debug!("Published to MQTT topic: {}", topic_clone);
                }

                #[cfg(feature = "defmt")]
                defmt::info!("MQTT outbound publisher stopped for topic: {}", topic_clone);
            }))?;
        }

        Ok(())
    }
}

/// Implement the Connector trait for MqttConnectorImpl
impl aimdb_core::transport::Connector for MqttConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        config: &aimdb_core::transport::ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Destination is the MQTT topic
        let topic = destination.to_string();
        let payload_owned = payload.to_vec();
        let qos = Self::map_qos(config.qos);
        let retain = config.retain;
        let action_sender = self.action_sender;

        Box::pin(SendFutureWrapper(async move {
            #[cfg(feature = "defmt")]
            defmt::debug!(
                "Publishing to MQTT topic '{}', {} bytes",
                topic,
                payload_owned.len()
            );

            // Create publish action
            let action = AimdbMqttAction::Publish {
                topic,
                payload: payload_owned,
                qos,
                retain,
            };

            // Send via channel (this will queue the publish)
            action_sender.send(action).await;

            #[cfg(feature = "defmt")]
            defmt::debug!("MQTT publish queued successfully");

            Ok(())
        }))
    }
}

impl MqttConnectorImpl {
    /// Map QoS level to mountain-mqtt QualityOfService
    fn map_qos(qos: u8) -> QualityOfService {
        match qos {
            0 => QualityOfService::Qos0,
            1 => QualityOfService::Qos1,
            2 => QualityOfService::Qos1, // Downgrade to QoS 1
            _ => QualityOfService::Qos0, // Default to QoS 0
        }
    }
}

// Helper wrapper to make futures Send for Embassy's single-threaded environment
//
// SAFETY: Embassy is single-threaded, so we can safely implement Send
// even though some Embassy types don't implement it. Embassy executors run
// cooperatively on a single core with no preemption or thread migration.
struct SendFutureWrapper<F>(F);

unsafe impl<F> Send for SendFutureWrapper<F> {}

impl<F: core::future::Future> core::future::Future for SendFutureWrapper<F> {
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        // SAFETY: We're just forwarding the poll call
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_mapping() {
        assert!(matches!(
            MqttConnectorImpl::map_qos(0),
            QualityOfService::Qos0
        ));
        assert!(matches!(
            MqttConnectorImpl::map_qos(1),
            QualityOfService::Qos1
        ));
        assert!(matches!(
            MqttConnectorImpl::map_qos(2),
            QualityOfService::Qos1
        )); // Downgrades to QoS 1
        assert!(matches!(
            MqttConnectorImpl::map_qos(99),
            QualityOfService::Qos0
        )); // Defaults to QoS 0
    }
}
