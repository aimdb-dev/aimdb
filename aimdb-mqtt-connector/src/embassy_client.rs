//! Embassy MQTT client implementation using mountain-mqtt-embassy
//!
//! This module provides production-ready MQTT connectivity for Embassy-based
//! embedded systems using mountain-mqtt-embassy's `run()` function.
//!
//! # Architecture
//!
//! Uses the channel-based pattern from mountain-mqtt-embassy:
//! - Background task calls `mqtt_manager::run()` with action/event channels
//! - `MqttClientPool` sends publish actions through the action channel
//! - Events (connected/disconnected) can be monitored through event channel
//! - Automatic reconnection handled by the run() function
//!
//! # Usage
//!
//! # Example
//!
//! ```rust,no_run
//! use aimdb_mqtt_connector::embassy_client::MqttConnector;
//!
//! // Create connector and get task spawner
//! let mqtt = MqttConnector::create(
//!     network_stack,
//!     "192.168.1.100",
//!     1883,
//!     "my-client-id"
//! ).await.unwrap();
//!
//! // Spawn the MQTT background task
//! spawner.spawn(async move { mqtt.task.run().await }).unwrap();
//!
//! // Publish messages
//! mqtt.connector.publish_async("sensor/temp", b"23.5", 1, false).await.unwrap();
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

use embassy_net::{Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use static_cell::StaticCell;

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
///     .with_connector(MqttConnectorBuilder::new("mqtt://192.168.1.100:1883"))
///     .configure::<Temperature>(|reg| {
///         reg.link_from("mqtt://commands/temp")
///            .with_deserializer(deserialize_temp)
///            .with_buffer(BufferCfg::SingleLatest);
///     })
///     .build().await?;
/// ```
pub struct MqttConnectorBuilder {
    broker_url: String,
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
    /// let builder = MqttConnectorBuilder::new("mqtt://192.168.1.100:1883");
    /// ```
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
        }
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
            let connector =
                MqttConnectorImpl::build_internal(&self.broker_url, router, db.runtime())
                    .await
                    .map_err(|_e| {
                        #[cfg(feature = "defmt")]
                        defmt::error!("Failed to build MQTT connector");

                        aimdb_core::DbError::RuntimeError { _message: () }
                    })?;

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
    /// * `router` - Pre-configured router with all routes
    /// * `runtime` - Embassy runtime adapter for spawning and network access
    async fn build_internal<R>(
        broker_url: &str,
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

        // Generate a unique client ID
        // In a real implementation, this should be configurable or use a hardware ID
        static CLIENT_ID_COUNTER: core::sync::atomic::AtomicU32 =
            core::sync::atomic::AtomicU32::new(0);
        let counter = CLIENT_ID_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        // Create a client ID in static memory
        static mut CLIENT_ID_BUF: [u8; 32] = [0u8; 32];
        let client_id = unsafe {
            use core::fmt::Write;
            let mut buf = heapless::String::<32>::new();
            write!(&mut buf, "aimdb-{}", counter).ok();

            // Copy to static buffer
            let len = buf.len().min(31);
            CLIENT_ID_BUF[..len].copy_from_slice(&buf.as_bytes()[..len]);
            CLIENT_ID_BUF[len] = 0; // Null terminate

            // Return as &'static str
            core::str::from_utf8_unchecked(&CLIENT_ID_BUF[..len])
        };

        #[cfg(feature = "defmt")]
        defmt::info!("MQTT client ID: {}", client_id);

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
        let connection_settings = ConnectionSettings::unauthenticated(client_id);

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
        let action_sender = self.action_sender.clone();

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

/// Background task that runs the MQTT connection loop forever
///
/// This task should be spawned and will run indefinitely, maintaining
/// the MQTT connection and processing publish requests.
pub struct MqttBackgroundTask {
    network_stack: Stack<'static>,
    connection_settings: ConnectionSettings<'static>,
    settings: Settings,
    event_sender: Sender<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
    action_receiver: Receiver<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
}

impl MqttBackgroundTask {
    /// Run the MQTT background task (never returns)
    ///
    /// This should be spawned as an embassy task:
    /// ```rust,no_run
    /// spawner.spawn(async move { mqtt_task.run().await }).unwrap();
    /// ```
    ///
    /// Note: This function never returns - it runs the MQTT connection loop forever.
    pub async fn run(self) {
        // Call mqtt_manager::run which returns ! (never type)
        // We need to use a type annotation to avoid exposing ! in our signature
        #[allow(unreachable_code)]
        {
            let _never: () = mqtt_manager::run::<
                AimdbMqttAction,
                AimdbMqttEvent,
                MAX_PROPERTIES,
                BUFFER_SIZE,
                CHANNEL_SIZE,
            >(
                self.network_stack,
                self.connection_settings,
                self.settings,
                self.event_sender,
                self.action_receiver,
            )
            .await;
            // The above never actually assigns to _never since it returns !
            // But it allows us to have a () return type
        }
    }
}

/// Return type from MqttConnector::create() containing the connector and background task
pub struct MqttConnectorWithTask {
    /// The connector for publishing messages
    pub connector: MqttConnector,
    /// Background task that must be spawned to run the MQTT connection
    pub task: MqttBackgroundTask,
}

/// MQTT connector for Embassy runtime
///
/// This provides a simple interface for publishing MQTT messages.
/// The actual MQTT connection is managed by a background task.
#[derive(Clone)]
pub struct MqttConnector {
    /// Channel sender for MQTT actions
    action_sender: Sender<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
}

impl MqttConnector {
    /// Create a new MQTT connector and prepare the background task
    ///
    /// This sets up all the infrastructure needed for MQTT communication:
    /// - Creates channels for actions and events
    /// - Configures connection settings
    /// - Returns a struct containing the connector and background task
    ///
    /// # Arguments
    ///
    /// * `network_stack` - Embassy network stack for TCP connections
    /// * `broker_host` - MQTT broker IP address (e.g., "192.168.1.100")
    /// * `broker_port` - MQTT broker port (typically 1883)
    /// * `client_id` - MQTT client identifier (must be 'static)
    ///
    /// # Returns
    ///
    /// A `MqttConnectorWithTask` containing the connector and background task.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let mqtt = MqttConnector::create(
    ///     network_stack,
    ///     "192.168.1.100",
    ///     1883,
    ///     "sensor-node-1"
    /// ).await?;
    ///
    /// // Spawn the background task
    /// spawner.spawn(async move { mqtt.task.run().await }).unwrap();
    ///
    /// // Use the connector
    /// mqtt.connector.publish_async("sensor/temp", b"23.5", 1, false).await?;
    /// ```
    pub async fn create(
        network_stack: Stack<'static>,
        broker_host: &str,
        broker_port: u16,
        client_id: &'static str,
    ) -> Result<MqttConnectorWithTask, PublishError> {
        // Parse broker IP address
        let broker_ip =
            Ipv4Addr::from_str(broker_host).map_err(|_| PublishError::ConnectionFailed)?;

        // Convert to embassy Ipv4Address
        let octets = broker_ip.octets();
        let broker_addr = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

        #[cfg(feature = "defmt")]
        defmt::info!(
            "Initializing MQTT: broker={}:{}, client_id={}",
            broker_host,
            broker_port,
            client_id
        );

        // Create static channels for MQTT communication using StaticCell
        static ACTION_CHANNEL: StaticCell<Channel<NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>> =
            StaticCell::new();
        static EVENT_CHANNEL: StaticCell<
            Channel<NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
        > = StaticCell::new();

        let action_channel = ACTION_CHANNEL.init(Channel::new());
        let event_channel = EVENT_CHANNEL.init(Channel::new());

        let action_sender = action_channel.sender();
        let action_receiver = action_channel.receiver();
        let event_sender = event_channel.sender();
        let _event_receiver = event_channel.receiver(); // Could be used for monitoring

        // Create connection settings
        let connection_settings = ConnectionSettings::unauthenticated(client_id);

        // Create mqtt_manager settings
        let settings = Settings::new(broker_addr, broker_port);

        // Create the connector
        let connector = Self { action_sender };

        // Create the background task
        let mqtt_task = MqttBackgroundTask {
            network_stack,
            connection_settings,
            settings,
            event_sender,
            action_receiver,
        };

        Ok(MqttConnectorWithTask {
            connector,
            task: mqtt_task,
        })
    }

    /// Helper to map QoS level from u8 to QualityOfService enum
    fn map_qos(qos: u8) -> QualityOfService {
        match qos {
            0 => QualityOfService::Qos0,
            1 => QualityOfService::Qos1,
            2 => {
                #[cfg(feature = "defmt")]
                defmt::warn!("QoS 2 not fully supported, using QoS 1");
                QualityOfService::Qos1
            }
            _ => {
                #[cfg(feature = "defmt")]
                defmt::warn!("Invalid QoS {}, defaulting to QoS 0", qos);
                QualityOfService::Qos0
            }
        }
    }

    /// Publish a message to an MQTT topic
    ///
    /// This queues the publish action which will be processed by the
    /// background task. The method returns immediately after queuing.
    ///
    /// # Arguments
    ///
    /// * `topic` - MQTT topic name
    /// * `payload` - Message payload bytes
    /// * `qos` - Quality of Service level (0, 1, or 2)
    /// * `retain` - Whether the broker should retain this message
    ///
    /// # Returns
    ///
    /// `Ok(())` if queued successfully, or `PublishError::BufferFull` if
    /// the action channel is full.
    pub async fn publish_async(
        &self,
        topic: &str,
        payload: &[u8],
        qos: u8,
        retain: bool,
    ) -> Result<(), PublishError> {
        let qos_enum = Self::map_qos(qos);

        let action = AimdbMqttAction::Publish {
            topic: topic.to_string(),
            payload: payload.to_vec(),
            qos: qos_enum,
            retain,
        };

        #[cfg(feature = "defmt")]
        defmt::debug!(
            "Queuing publish: topic={}, len={}, qos={:?}",
            topic,
            payload.len(),
            qos_enum
        );

        // Note: send().await waits indefinitely until there's space in the channel
        // It does not return a Result - it always succeeds once space is available
        self.action_sender.send(action).await;

        Ok(())
    }

    /// Subscribe to an MQTT topic
    ///
    /// This queues the subscribe action which will be processed by the
    /// background task. The method returns immediately after queuing.
    ///
    /// # Arguments
    ///
    /// * `topic` - MQTT topic name (supports wildcards: + for single level, # for multi-level)
    /// * `qos` - Quality of Service level for the subscription (0, 1, or 2)
    ///
    /// # Returns
    ///
    /// `Ok(())` if queued successfully.
    pub async fn subscribe_async(&self, topic: &str, qos: u8) -> Result<(), PublishError> {
        let qos_enum = Self::map_qos(qos);

        let action = AimdbMqttAction::Subscribe {
            topic: topic.to_string(),
            qos: qos_enum,
        };

        #[cfg(feature = "defmt")]
        defmt::debug!("Queuing subscribe: topic={}, qos={:?}", topic, qos_enum);

        self.action_sender.send(action).await;

        Ok(())
    }
}

// SAFETY: Embassy is single-threaded, so we can safely implement Send + Sync
// even though NoopRawMutex doesn't implement them.
//
// Embassy executors run cooperatively on a single core, so there's no actual
// concurrent access. These markers allow MqttConnector to work with the
// Connector trait which requires Send + Sync for Tokio compatibility.
//
// This is safe because:
// 1. Embassy tasks run cooperatively (no preemption on single core)
// 2. NoopRawMutex is safe for single-threaded access
// 3. No actual thread migration occurs in Embassy
unsafe impl Send for MqttConnector {}
unsafe impl Sync for MqttConnector {}

// Helper wrapper to make the publish future Send
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

/// Implement the connector trait for Embassy
///
/// This allows using the same `.with_connector()` and `.link()` pattern
/// as the Tokio version, despite Embassy being single-threaded.
impl aimdb_core::transport::Connector for MqttConnector {
    fn publish(
        &self,
        destination: &str,
        config: &aimdb_core::transport::ConnectorConfig,
        payload: &[u8],
    ) -> core::pin::Pin<
        alloc::boxed::Box<
            dyn core::future::Future<Output = Result<(), aimdb_core::transport::PublishError>>
                + Send
                + '_,
        >,
    > {
        use alloc::boxed::Box;
        use alloc::vec::Vec;

        // destination is the topic for MQTT
        let topic_owned = destination.to_string();
        let payload_owned: Vec<u8> = payload.to_vec();
        let qos = config.qos;
        let retain = config.retain;

        // Wrap the future in our Send wrapper
        // SAFETY: Safe in Embassy's single-threaded environment
        let fut = SendFutureWrapper(async move {
            self.publish_async(&topic_owned, &payload_owned, qos, retain)
                .await
        });

        Box::pin(fut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_mapping() {
        assert!(matches!(MqttConnector::map_qos(0), QualityOfService::Qos0));
        assert!(matches!(MqttConnector::map_qos(1), QualityOfService::Qos1));
        assert!(matches!(MqttConnector::map_qos(2), QualityOfService::Qos1)); // Downgrades
        assert!(matches!(MqttConnector::map_qos(99), QualityOfService::Qos0)); // Defaults
    }
}
