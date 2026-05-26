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
        is_retry: bool,
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
                        defmt::debug!("Retrying publish to {}", topic.as_str());
                    } else {
                        defmt::debug!(
                            "Publishing {} bytes to {} (QoS={:?})",
                            payload.len(),
                            topic.as_str(),
                            qos
                        );
                    }
                }

                #[cfg(not(feature = "defmt"))]
                let _ = is_retry;

                client.publish(topic, payload, *qos, *retain).await?;

                #[cfg(feature = "defmt")]
                defmt::info!("Published {} bytes to {}", payload.len(), topic.as_str());

                Ok(())
            }
            Self::Subscribe { topic, qos } => {
                #[cfg(feature = "defmt")]
                {
                    if is_retry {
                        defmt::debug!("Retrying subscribe to {} (QoS={:?})", topic.as_str(), qos);
                    } else {
                        defmt::info!("Subscribing to {} (QoS={:?})", topic.as_str(), qos);
                    }
                }

                #[cfg(not(feature = "defmt"))]
                let _ = is_retry;

                client.subscribe(topic, *qos).await?;

                #[cfg(feature = "defmt")]
                defmt::info!("Subscribed to {}", topic.as_str());

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

type EmbassyBoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Implement ConnectorBuilder trait for Embassy runtime with network stack access.
///
/// Requires the runtime to provide the `EmbassyNetwork` trait so the connector
/// can access the network stack for creating TCP connections.
impl<R> ConnectorBuilder<R> for MqttConnectorBuilder
where
    R: aimdb_executor::RuntimeAdapter + aimdb_embassy_adapter::EmbassyNetwork + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>>
    {
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

            // Build the action sender + infrastructure futures (mqtt manager + event router).
            let runtime_ctx = db.runtime_any();
            let (action_sender, infra_futures) = MqttConnectorImpl::build_internal(
                &self.broker_url,
                &self.client_id,
                router,
                db.runtime(),
                Some(runtime_ctx),
            )
            .await
            .map_err(|_e| {
                #[cfg(feature = "defmt")]
                defmt::error!("Failed to build MQTT connector");

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

            // Collect outbound publisher futures.
            let outbound_routes = db.collect_outbound_routes("mqtt");

            #[cfg(feature = "defmt")]
            defmt::info!(
                "Collected {} outbound routes for MQTT connector",
                outbound_routes.len()
            );

            let runtime_ctx: Arc<dyn core::any::Any + Send + Sync> = db.runtime_any();
            let outbound_futures = MqttConnectorImpl::collect_outbound_futures(
                action_sender,
                runtime_ctx,
                outbound_routes,
            );

            let mut all: Vec<EmbassyBoxFuture> =
                Vec::with_capacity(infra_futures.len() + outbound_futures.len());
            all.extend(infra_futures);
            all.extend(outbound_futures);
            Ok(all)
        }))
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Build-time helper aggregating Embassy MQTT construction logic.
pub struct MqttConnectorImpl;

impl MqttConnectorImpl {
    /// Build the MQTT manager + event router futures and return the action
    /// channel sender for use by outbound publishers.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port)
    /// * `client_id` - MQTT client identifier
    /// * `router` - Pre-configured router with all routes
    /// * `runtime` - Embassy runtime adapter for network access
    /// * `runtime_ctx` - Optional type-erased runtime for context-aware deserializers
    async fn build_internal<R>(
        broker_url: &str,
        client_id: &str,
        router: Router,
        runtime: &R,
        runtime_ctx: Option<Arc<dyn core::any::Any + Send + Sync>>,
    ) -> Result<
        (
            Sender<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
            Vec<EmbassyBoxFuture>,
        ),
        &'static str,
    >
    where
        R: aimdb_executor::RuntimeAdapter + aimdb_embassy_adapter::EmbassyNetwork + 'static,
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
        defmt::info!("Creating MQTT connector for {}:{}", host.as_str(), port);

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

        // Build the MQTT manager future (returned to the runner — design 028 §"Connector futures").
        let mqtt_task_future: EmbassyBoxFuture = Box::pin(SendFutureWrapper(async move {
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
        }));

        // Build the event router future for inbound message routing.
        let event_router_future: EmbassyBoxFuture = Box::pin(SendFutureWrapper(async move {
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
                            topic.as_str(),
                            payload.len()
                        );

                        // Route the message through the router to the appropriate producer
                        if let Err(_e) = router_for_task
                            .route(&topic, &payload, runtime_ctx.as_ref())
                            .await
                        {
                            #[cfg(feature = "defmt")]
                            defmt::warn!("Failed to route MQTT message from '{}'", topic.as_str());
                        }
                    }
                    _ => {
                        // Ignore other events (Connected, Disconnected, etc.)
                        #[cfg(feature = "defmt")]
                        defmt::trace!("Ignoring MQTT event (not a message)");
                    }
                }
            }
        }));

        // Subscribe to all topics from the router. The subscribe actions are
        // queued in the action channel; the MQTT manager future (not yet
        // driven) will drain them once `AimDbRunner::run()` polls it.
        for topic in &topics {
            #[cfg(feature = "defmt")]
            defmt::info!("Subscribing to MQTT topic: {}", &**topic);

            let subscribe_action = AimdbMqttAction::Subscribe {
                topic: topic.to_string(),
                qos: QualityOfService::Qos1, // Use QoS 1 for reliable delivery
            };

            action_sender.send(subscribe_action).await;
        }

        #[cfg(feature = "defmt")]
        defmt::info!("Queued subscriptions for {} topics", topics.len());

        let _ = router_arc; // router_arc lives inside event_router_future via clone

        Ok((
            action_sender,
            alloc::vec![mqtt_task_future, event_router_future],
        ))
    }

    /// Collects outbound publisher futures for all configured routes (internal).
    ///
    /// Each future subscribes to a typed record, serializes values, and sends
    /// them to the MQTT action channel. Returned futures are appended to the
    /// `AimDbRunner` accumulator.
    fn collect_outbound_futures(
        action_sender: Sender<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
        runtime_ctx: Arc<dyn core::any::Any + Send + Sync>,
        routes: Vec<aimdb_core::OutboundRoute>,
    ) -> Vec<EmbassyBoxFuture> {
        let mut futures: Vec<EmbassyBoxFuture> = Vec::with_capacity(routes.len());

        for (default_topic, consumer, serializer, config, topic_provider) in routes {
            let default_topic_clone = default_topic.clone();
            let runtime_ctx = runtime_ctx.clone();

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

            futures.push(Box::pin(SendFutureWrapper(async move {
                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "defmt")]
                        defmt::error!(
                            "Failed to subscribe for outbound topic '{}'",
                            default_topic_clone.as_str()
                        );
                        return;
                    }
                };

                #[cfg(feature = "defmt")]
                defmt::info!(
                    "MQTT outbound publisher started for topic: {}",
                    default_topic_clone.as_str()
                );

                while let Ok(value_any) = reader.recv_any().await {
                    // Determine topic: dynamic (from provider) or default (from URL)
                    let topic = topic_provider
                        .as_ref()
                        .and_then(|provider| provider.topic_any(&*value_any))
                        .unwrap_or_else(|| default_topic_clone.clone());

                    // Serialize the type-erased value
                    let bytes = match &serializer {
                        aimdb_core::connector::SerializerKind::Raw(ser) => match ser(&*value_any) {
                            Ok(b) => b,
                            Err(_e) => {
                                #[cfg(feature = "defmt")]
                                defmt::error!("Failed to serialize for topic '{}'", topic.as_str());
                                continue;
                            }
                        },
                        aimdb_core::connector::SerializerKind::Context(ser) => {
                            match ser(runtime_ctx.clone(), &*value_any) {
                                Ok(b) => b,
                                Err(_e) => {
                                    #[cfg(feature = "defmt")]
                                    defmt::error!(
                                        "Failed to serialize for topic '{}'",
                                        topic.as_str()
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    // Publish to MQTT via action channel
                    let action = AimdbMqttAction::Publish {
                        topic: topic.clone(),
                        payload: bytes,
                        qos,
                        retain,
                    };

                    action_sender.send(action).await;

                    #[cfg(feature = "defmt")]
                    defmt::debug!("Published to MQTT topic: {}", topic.as_str());
                }

                #[cfg(feature = "defmt")]
                defmt::info!(
                    "MQTT outbound publisher stopped for topic: {}",
                    default_topic_clone.as_str()
                );
            })));
        }

        futures
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
