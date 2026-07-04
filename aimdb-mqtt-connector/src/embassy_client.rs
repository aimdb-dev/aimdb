//! Embassy MQTT client implementation using mountain-mqtt-embassy
//!
//! This module provides production-ready MQTT connectivity for Embassy-based
//! embedded systems using mountain-mqtt-embassy's `run()` function.
//!
//! # Architecture
//!
//! The data-flow (outbound publish, inbound routing) rides core's
//! [`pump_sink`](aimdb_core::session::pump_sink) /
//! [`pump_source`](aimdb_core::session::pump_source) via the force-`Send`
//! [`EmbassySink`]/[`EmbassySource`] bridges in `aimdb-embassy-adapter`, exactly
//! like the Tokio half rides them. This crate contributes only the
//! transport-specific bits: the broker **manager task** (mountain-mqtt's `run`),
//! the [`MqttSink`]/[`MqttSource`] over its action/event channels, and the
//! `MqttOperations`/`FromApplicationMessage` glue. The single `unsafe` block
//! is the [`NetStack`](aimdb_embassy_adapter::connectors::NetStack)
//! construction in [`MqttConnectorBuilder::new`], acknowledging the adapter's
//! single-core executor invariant.
//!
//! # Usage
//!
//! Illustrative (not compiled: requires the `embassy-runtime` feature and a
//! device network stack):
//!
//! ```rust,ignore
//! use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
//! use aimdb_core::AimDbBuilder;
//!
//! // `stack: &'static embassy_net::Stack<'static>` — the device's network stack.
//! let db = AimDbBuilder::new()
//!     .runtime(embassy_adapter)
//!     .with_connector(
//!         MqttConnectorBuilder::new("mqtt://192.168.1.100:1883", stack)
//!             .with_client_id("my-unique-device-id"),
//!     )
//!     .configure::<Temperature>("temperature", |reg| {
//!         reg.link_to("mqtt://sensors/temperature").finish();
//!         reg.link_from("mqtt://commands/temperature").finish();
//!     })
//!     .build().await?;
//! ```

extern crate alloc;

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::RouterBuilder;
use aimdb_core::session::{pump_sink, pump_source, Payload};
use aimdb_core::transport::{ConnectorConfig, PublishError};
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

use aimdb_embassy_adapter::connectors::{
    into_box_future, EmbassySink, EmbassySinkRaw, EmbassySource, EmbassySourceRaw,
};
use embassy_net::Ipv4Address;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
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

/// The runner's collected future type.
type EmbassyBoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Sender half of the action channel (outbound publishes + subscriptions).
type ActionSender = Sender<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>;
/// Receiver half of the event channel (inbound messages from the broker).
type EventReceiver = Receiver<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>;

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
/// record producers via core's `pump_source`.
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

// ===========================================================================
// Data-plane bridges — ride core's pumps via the adapter's force-`Send` wrappers.
// ===========================================================================

/// Outbound sink: turns a `pump_sink` publish into an `AimdbMqttAction::Publish`
/// enqueued onto the manager's action channel. Wrapped in
/// [`EmbassySink`] so it drives core's `pump_sink` despite the `!Send` channel.
struct MqttSink {
    sender: ActionSender,
}

impl EmbassySinkRaw for MqttSink {
    async fn publish(
        &self,
        destination: String,
        config: ConnectorConfig,
        payload: Vec<u8>,
    ) -> Result<(), PublishError> {
        // `qos`/`retain` arrive via the URL query (passed through in
        // `protocol_options`); default to QoS 1 (legacy behaviour), no retain.
        let qos = opt_u8(&config, "qos")
            .map(map_qos)
            .unwrap_or(QualityOfService::Qos1);
        let retain = opt_bool(&config, "retain").unwrap_or(false);

        self.sender
            .send(AimdbMqttAction::Publish {
                topic: destination,
                payload,
                qos,
                retain,
            })
            .await;
        Ok(())
    }
}

/// Inbound source: drains the manager's event channel, yielding each received
/// message as `(topic, payload)`. Wrapped in [`EmbassySource`] so it drives
/// core's `pump_source` (which fans out to the matching record producers).
struct MqttSource {
    receiver: EventReceiver,
}

impl EmbassySourceRaw for MqttSource {
    async fn next(&mut self) -> Option<(String, Payload)> {
        loop {
            match self.receiver.receive().await {
                MqttEvent::ApplicationEvent {
                    event: AimdbMqttEvent::MessageReceived { topic, payload },
                    ..
                } => return Some((topic, Payload::from(payload))),
                // Connection lifecycle events (Connected/Disconnected/…) carry no
                // record data; skip and keep draining.
                _ => continue,
            }
        }
    }
}

/// MQTT connector builder for Embassy with router-based dispatch.
///
/// Collects routes from the database during `build()` and wires the broker
/// manager + the outbound/inbound pumps.
pub struct MqttConnectorBuilder {
    broker_url: String,
    client_id: String,
    stack: aimdb_embassy_adapter::connectors::NetStack,
}

impl MqttConnectorBuilder {
    /// Create a new MQTT connector builder for Embassy.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL in format `mqtt://host:port`
    /// * `stack` - The device's network stack (the runtime travels as
    ///   `Arc<dyn RuntimeOps>` and cannot surface it)
    pub fn new(broker_url: impl Into<String>, stack: &'static embassy_net::Stack<'static>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: "aimdb-client".to_string(),
            // SAFETY: AimDB's Embassy integration requires a single-core
            // cooperative executor (the adapter's module-level invariant);
            // every future touching this stack — including the broker task
            // built from this builder — is polled on that executor.
            stack: unsafe { aimdb_embassy_adapter::connectors::NetStack::new(stack) },
        }
    }

    /// Set the MQTT client ID (should be unique per device).
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = client_id.into();
        self
    }
}

/// Implement ConnectorBuilder trait for Embassy.
///
/// The network stack is taken at construction (see
/// [`MqttConnectorBuilder::new`]), so the builder needs nothing from the
/// runtime beyond the dyn-safe capabilities the database already holds.
impl ConnectorBuilder for MqttConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>>
    {
        // No `.await` in this body, so the future is `Send` without a wrapper: the
        // `!Send` channel ends are immediately moved into the force-`Send`
        // `EmbassySink`/`EmbassySource`/manager-task and never held across a suspend.
        Box::pin(async move {
            // Inbound topics to subscribe to (the manager sends `Subscribe` for each).
            let inbound_routes = db.collect_inbound_routes("mqtt");
            let topics: Vec<String> = RouterBuilder::from_routes(inbound_routes)
                .build()
                .resource_ids()
                .iter()
                .map(|t| t.to_string())
                .collect();

            #[cfg(feature = "defmt")]
            defmt::info!("MQTT: subscribing to {} inbound topics", topics.len());

            // Broker manager task + the channel ends for the pumps.
            let (action_sender, event_receiver, manager_task) =
                setup_manager(&self.broker_url, &self.client_id, self.stack, topics)?;

            // Outbound publishes + inbound routing ride core's pumps.
            let mut futures = pump_sink(
                db,
                "mqtt",
                Arc::new(EmbassySink(MqttSink {
                    sender: action_sender,
                })),
            );
            futures.extend(pump_source(
                db,
                "mqtt",
                EmbassySource(MqttSource {
                    receiver: event_receiver,
                }),
            ));
            // The broker manager protocol loop (force-`Send` via the adapter).
            futures.push(manager_task);

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Set up the static channels + the mountain-mqtt broker manager task, returning
/// the action sender (outbound), the event receiver (inbound), and the manager
/// task future (which first queues the topic subscriptions, then runs the broker
/// loop). Synchronous — no `.await` — so the caller's `build` future stays `Send`.
fn setup_manager(
    broker_url: &str,
    client_id: &str,
    stack: aimdb_embassy_adapter::connectors::NetStack,
    topics: Vec<String>,
) -> Result<(ActionSender, EventReceiver, EmbassyBoxFuture), aimdb_core::DbError> {
    let build_err = |msg: &str| {
        #[cfg(feature = "defmt")]
        defmt::error!("Failed to build MQTT connector: {}", msg);
        aimdb_core::DbError::runtime_error(format!("Failed to build MQTT connector: {}", msg))
    };

    // Parse the broker URL (add a dummy topic if none, so parsing succeeds).
    let mut url = broker_url.to_string();
    if !url.contains('/') || url.matches('/').count() < 3 {
        url = format!("{}/dummy", url.trim_end_matches('/'));
    }
    let connector_url = ConnectorUrl::parse(&url).map_err(|_| build_err("Invalid MQTT URL"))?;
    let host = connector_url.host.clone();
    let port = connector_url.port.unwrap_or(1883);

    let broker_ip =
        Ipv4Addr::from_str(&host).map_err(|_| build_err("Invalid broker IP address"))?;
    let octets = broker_ip.octets();
    let broker_addr = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

    // Store client_id in static memory for the 'static lifetime requirement.
    static CLIENT_ID_STORAGE: OnceLock<String> = OnceLock::new();
    let client_id_static: &'static str = CLIENT_ID_STORAGE.get_or_init(|| client_id.to_string());

    // Static channels for MQTT communication.
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
    let event_receiver = event_channel.receiver();

    let connection_settings = ConnectionSettings::unauthenticated(client_id_static);
    let settings = Settings::new(broker_addr, port);
    let network = stack.get();

    // Manager task: queue subscriptions, then run the broker loop (never returns).
    let sub_sender = action_sender;
    let manager_task = into_box_future(async move {
        for topic in &topics {
            sub_sender
                .send(AimdbMqttAction::Subscribe {
                    topic: topic.clone(),
                    qos: QualityOfService::Qos1,
                })
                .await;
        }

        #[cfg(feature = "defmt")]
        defmt::info!("MQTT background task starting");

        #[allow(unreachable_code)]
        {
            let _: () = mqtt_manager::run::<
                AimdbMqttAction,
                AimdbMqttEvent,
                MAX_PROPERTIES,
                BUFFER_SIZE,
                CHANNEL_SIZE,
            >(
                *network,
                connection_settings,
                settings,
                event_sender,
                action_receiver,
            )
            .await;
        }
    });

    Ok((action_sender, event_receiver, manager_task))
}

/// Map a QoS level (0/1/2) to mountain-mqtt's `QualityOfService` (2 downgrades to 1).
fn map_qos(qos: u8) -> QualityOfService {
    match qos {
        0 => QualityOfService::Qos0,
        1 => QualityOfService::Qos1,
        2 => QualityOfService::Qos1, // Downgrade to QoS 1
        _ => QualityOfService::Qos0, // Default to QoS 0
    }
}

/// Read a `u8` option from the per-route `protocol_options` (URL query).
fn opt_u8(config: &ConnectorConfig, key: &str) -> Option<u8> {
    config
        .protocol_options
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse::<u8>().ok())
}

/// Read a `bool` option from the per-route `protocol_options` (URL query).
fn opt_bool(config: &ConnectorConfig, key: &str) -> Option<bool> {
    config
        .protocol_options
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse::<bool>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_mapping() {
        assert!(matches!(map_qos(0), QualityOfService::Qos0));
        assert!(matches!(map_qos(1), QualityOfService::Qos1));
        assert!(matches!(map_qos(2), QualityOfService::Qos1)); // Downgrades to QoS 1
        assert!(matches!(map_qos(99), QualityOfService::Qos0)); // Defaults to QoS 0
    }
}
