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

#[cfg(feature = "embassy-tls")]
pub use crate::embassy_tls::TlsOptions;
#[cfg(feature = "embassy-tls")]
use crate::embassy_tls::{host_is_ip_literal, run_tls};

/// Maximum number of pending MQTT actions and events
pub(crate) const CHANNEL_SIZE: usize = 32;

/// Buffer size for MQTT packets (4KB)
pub(crate) const BUFFER_SIZE: usize = 4096;

/// Maximum properties in MQTT packets
pub(crate) const MAX_PROPERTIES: usize = 16;

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

/// Force-`Send + Sync` slot for the TLS materials: [`TlsOptions`] holds
/// `&'static mut` exclusive resources (TRNG, record buffers), so it is
/// neither `Sync` nor takeable through the `&self` that
/// [`ConnectorBuilder::build`] receives without interior mutability.
///
/// SAFETY invariant: same single-core cooperative-executor invariant as
/// [`NetStack`](aimdb_embassy_adapter::connectors::NetStack); the slot is
/// written by `with_tls` and taken exactly once inside `build()`, both on
/// that executor.
#[cfg(feature = "embassy-tls")]
struct TlsSlot(core::cell::RefCell<Option<TlsOptions>>);

// SAFETY: see the struct-level invariant.
#[cfg(feature = "embassy-tls")]
unsafe impl Send for TlsSlot {}
// SAFETY: see the struct-level invariant.
#[cfg(feature = "embassy-tls")]
unsafe impl Sync for TlsSlot {}

/// MQTT connector builder for Embassy with router-based dispatch.
///
/// Collects routes from the database during `build()` and wires the broker
/// manager + the outbound/inbound pumps. The broker URL scheme selects the
/// transport: `mqtt://` is plain TCP (default port 1883), `mqtts://` is TLS
/// (default port 8883) and requires both the `embassy-tls` feature and
/// [`with_tls`](Self::with_tls).
pub struct MqttConnectorBuilder {
    broker_url: String,
    client_id: String,
    credentials: Option<(String, String)>,
    #[cfg(feature = "embassy-tls")]
    tls: TlsSlot,
    stack: aimdb_embassy_adapter::connectors::NetStack,
}

impl MqttConnectorBuilder {
    /// Create a new MQTT connector builder for Embassy.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL in format `mqtt://host:port` (plain TCP)
    ///   or `mqtts://host:port` (TLS, see [`with_tls`](Self::with_tls))
    /// * `stack` - The device's network stack (the runtime travels as
    ///   `Arc<dyn RuntimeOps>` and cannot surface it)
    pub fn new(broker_url: impl Into<String>, stack: &'static embassy_net::Stack<'static>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: "aimdb-client".to_string(),
            credentials: None,
            #[cfg(feature = "embassy-tls")]
            tls: TlsSlot(core::cell::RefCell::new(None)),
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

    /// Authenticate with the broker (MQTT CONNECT username/password).
    ///
    /// Works on both transports, but note that over `mqtt://` the credential
    /// transits in cleartext — pair it with `mqtts://` outside a trusted LAN.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.credentials = Some((username.into(), password.into()));
        self
    }

    /// Provide the TLS materials for an `mqtts://` broker.
    ///
    /// Required for `mqtts://` URLs; rejected at `build()` for `mqtt://`.
    #[cfg(feature = "embassy-tls")]
    pub fn with_tls(self, options: TlsOptions) -> Self {
        *self.tls.0.borrow_mut() = Some(options);
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

            let broker = parse_broker_url(&self.broker_url)?;
            let connection_settings =
                static_connection_settings(&self.client_id, self.credentials.as_ref());

            // Broker manager task(s) + the channel ends for the pumps.
            // The URL scheme selects the transport.
            #[cfg(feature = "embassy-tls")]
            let (action_sender, event_receiver, manager_tasks) = {
                let tls_options = self.tls.0.borrow_mut().take();
                match (broker.tls, tls_options) {
                    (true, Some(options)) => setup_tls_manager(
                        &broker,
                        options,
                        connection_settings,
                        self.stack,
                        topics,
                    )?,
                    (true, None) => {
                        return Err(build_err("mqtts:// broker URLs require .with_tls(...)"))
                    }
                    (false, Some(_)) => {
                        return Err(build_err(".with_tls(...) requires an mqtts:// broker URL"))
                    }
                    (false, None) => {
                        setup_manager(&broker, connection_settings, self.stack, topics)?
                    }
                }
            };
            #[cfg(not(feature = "embassy-tls"))]
            let (action_sender, event_receiver, manager_tasks) = {
                if broker.tls {
                    return Err(build_err(
                        "mqtts:// broker URLs require the `embassy-tls` feature of aimdb-mqtt-connector",
                    ));
                }
                setup_manager(&broker, connection_settings, self.stack, topics)?
            };

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
            // The broker manager protocol loop (plus the SNTP time-source
            // task on the TLS path), force-`Send` via the adapter.
            futures.extend(manager_tasks);

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Parsed broker endpoint: transport + authority.
struct BrokerUrl {
    tls: bool,
    host: String,
    port: u16,
}

fn build_err(msg: &str) -> aimdb_core::DbError {
    #[cfg(feature = "defmt")]
    defmt::error!("Failed to build MQTT connector: {}", msg);
    aimdb_core::DbError::runtime_error(format!("Failed to build MQTT connector: {}", msg))
}

/// Parse the broker URL into transport + host + port (`mqtt://` 1883,
/// `mqtts://` 8883).
fn parse_broker_url(broker_url: &str) -> Result<BrokerUrl, aimdb_core::DbError> {
    // Add a dummy topic if none, so parsing succeeds.
    let mut url = broker_url.to_string();
    if !url.contains('/') || url.matches('/').count() < 3 {
        url = format!("{}/dummy", url.trim_end_matches('/'));
    }
    let connector_url = ConnectorUrl::parse(&url).map_err(|_| build_err("Invalid MQTT URL"))?;
    let tls = match connector_url.scheme.as_str() {
        "mqtt" => false,
        "mqtts" => true,
        _ => return Err(build_err("Broker URL scheme must be mqtt:// or mqtts://")),
    };
    let port = connector_url.port.unwrap_or(if tls { 8883 } else { 1883 });
    Ok(BrokerUrl {
        tls,
        host: connector_url.host,
        port,
    })
}

/// Build the `ConnectionSettings<'static>` for MQTT CONNECT, parking the
/// identity strings in statics for the `'static` lifetime requirement.
fn static_connection_settings(
    client_id: &str,
    credentials: Option<&(String, String)>,
) -> ConnectionSettings<'static> {
    static CLIENT_ID_STORAGE: OnceLock<String> = OnceLock::new();
    static CREDENTIALS_STORAGE: OnceLock<(String, String)> = OnceLock::new();

    let client_id: &'static str = CLIENT_ID_STORAGE.get_or_init(|| client_id.to_string());
    match credentials {
        Some(credentials) => {
            let credentials: &'static (String, String) =
                CREDENTIALS_STORAGE.get_or_init(|| credentials.clone());
            ConnectionSettings::authenticated(
                client_id,
                credentials.0.as_str(),
                credentials.1.as_bytes(),
            )
        }
        None => ConnectionSettings::unauthenticated(client_id),
    }
}

/// Sender half of the event channel (used by the broker manager tasks).
type EventSender = Sender<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>;
/// Receiver half of the action channel (drained by the broker manager tasks).
type ActionReceiver = Receiver<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>;

/// Initialise the static action/event channels shared by both transports
/// (one MQTT connector per firmware — `StaticCell` enforces single init).
fn init_channels() -> (ActionSender, ActionReceiver, EventSender, EventReceiver) {
    static ACTION_CHANNEL: StaticCell<Channel<NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>> =
        StaticCell::new();
    static EVENT_CHANNEL: StaticCell<
        Channel<NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
    > = StaticCell::new();
    let action_channel = ACTION_CHANNEL.init(Channel::new());
    let event_channel = EVENT_CHANNEL.init(Channel::new());

    (
        action_channel.sender(),
        action_channel.receiver(),
        event_channel.sender(),
        event_channel.receiver(),
    )
}

/// Set up the plain-TCP broker manager (mountain-mqtt-embassy's `run`),
/// returning the action sender (outbound), the event receiver (inbound), and
/// the manager task future (which first queues the topic subscriptions, then
/// runs the broker loop). Synchronous — no `.await` — so the caller's `build`
/// future stays `Send`.
fn setup_manager(
    broker: &BrokerUrl,
    connection_settings: ConnectionSettings<'static>,
    stack: aimdb_embassy_adapter::connectors::NetStack,
    topics: Vec<String>,
) -> Result<(ActionSender, EventReceiver, Vec<EmbassyBoxFuture>), aimdb_core::DbError> {
    let broker_ip = Ipv4Addr::from_str(&broker.host).map_err(|_| {
        build_err("Invalid broker IP address (plain mqtt:// needs an IPv4 literal)")
    })?;
    let octets = broker_ip.octets();
    let broker_addr = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

    let (action_sender, action_receiver, event_sender, event_receiver) = init_channels();

    let settings = Settings::new(broker_addr, broker.port);
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

    Ok((action_sender, event_receiver, alloc::vec![manager_task]))
}

/// Set up the TLS broker manager ([`run_tls`]) plus the SNTP time-source
/// task. Synchronous — no `.await` — so the caller's `build` future stays
/// `Send`.
#[cfg(feature = "embassy-tls")]
fn setup_tls_manager(
    broker: &BrokerUrl,
    options: TlsOptions,
    connection_settings: ConnectionSettings<'static>,
    stack: aimdb_embassy_adapter::connectors::NetStack,
    topics: Vec<String>,
) -> Result<(ActionSender, EventReceiver, Vec<EmbassyBoxFuture>), aimdb_core::DbError> {
    if host_is_ip_literal(&broker.host) {
        return Err(build_err(
            "mqtts:// needs a hostname — certificate verification cannot match an IP literal",
        ));
    }

    let (action_sender, action_receiver, event_sender, event_receiver) = init_channels();

    // `Settings` supplies the session cadence and port; its address field is
    // unused on the TLS path (the host is resolved per attempt instead).
    let settings = Settings::new(Ipv4Address::UNSPECIFIED, broker.port);
    let network = stack.get();
    let host = broker.host.clone();
    let sntp_server = options.sntp_server;

    let manager_task = into_box_future(async move {
        #[cfg(feature = "defmt")]
        defmt::info!("MQTT-TLS background task starting");

        #[allow(unreachable_code)]
        {
            let _: () = run_tls(
                *network,
                options,
                host,
                topics,
                connection_settings,
                settings,
                event_sender,
                action_receiver,
            )
            .await;
        }
    });
    let sntp_task = into_box_future(async move {
        #[allow(unreachable_code)]
        {
            let _: () = crate::sntp::run(*network, sntp_server).await;
        }
    });

    Ok((
        action_sender,
        event_receiver,
        alloc::vec![manager_task, sntp_task],
    ))
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
