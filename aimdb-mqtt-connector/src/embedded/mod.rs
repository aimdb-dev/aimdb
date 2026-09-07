//! The `mountain-mqtt` backend: broker session plus the data-plane bridges.
//!
//! Outbound publishes and inbound routing ride core's [`pump_sink`] /
//! [`pump_source`] directly — the session channels are `Sync`, so nothing
//! force-`Send` stands between them and the runner. This module contributes
//! the connector builder, the `MqttSink`/`MqttSource` over those channels, and
//! the `MqttOperations`/`FromApplicationMessage` glue.
//!
//! # Usage
//!
//! ```rust,ignore
//! let db = AimDbBuilder::new()
//!     .runtime(embassy_adapter)
//!     .with_connector(
//!         MqttConnector::new("mqtt://192.168.1.100:1883")
//!             .transport(EmbassyNet::tcp(stack, rx, tx))
//!             .with_client_id("my-unique-device-id"),
//!     )
//!     .build()
//!     .await?;
//! ```

pub mod manager;
pub mod session;

// TLS transport + SNTP time source.
#[cfg(feature = "embassy-tls")]
pub mod sntp;
#[cfg(feature = "embassy-tls")]
pub mod tls;

extern crate alloc;

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::RouterBuilder;
use aimdb_core::session::{pump_sink, pump_source, Payload};
use aimdb_core::transport::{ConnectorConfig, PublishError};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::net::Ipv4Addr;
use core::pin::Pin;
use core::str::FromStr;

#[cfg(feature = "embassy-tls")]
use aimdb_embassy_adapter::connectors::into_box_future;

use mountain_mqtt::client::{Client, ClientError, ConnectionSettings};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::mqtt_manager::{ConnectionId, MqttOperations};

use crate::embedded::manager::{MqttEvent, Settings};

#[cfg(feature = "embassy-tls")]
pub use crate::embedded::tls::TlsOptions;
#[cfg(feature = "embassy-tls")]
use crate::embedded::tls::{host_ip_literal, run_tls, READ_BUF_MIN};

/// Maximum number of pending MQTT actions and events
pub(crate) const CHANNEL_SIZE: usize = 32;

/// Buffer size for MQTT packets (4KB)
pub(crate) const BUFFER_SIZE: usize = 4096;

/// Maximum properties in MQTT packets
pub(crate) const MAX_PROPERTIES: usize = 16;

/// The runner's collected future type.
type EmbassyBoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What a transport's setup hands back: the two channel ends the pumps ride,
/// plus the tasks that serve them.
type ManagerSetup = (Arc<ActionChannel>, Arc<EventChannel>, Vec<EmbassyBoxFuture>);

/// Outbound publishes and subscriptions: pumps to broker session.
pub(crate) type ActionChannel =
    crate::embedded::manager::ActionChannel<AimdbMqttAction, CHANNEL_SIZE>;
/// Inbound messages: broker session to pumps.
pub(crate) type EventChannel = crate::embedded::manager::EventChannel<AimdbMqttEvent, CHANNEL_SIZE>;

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
        /// The message payload, built once from the wire bytes.
        payload: Payload,
    },
}

impl crate::embedded::manager::FromApplicationMessage<MAX_PROPERTIES> for AimdbMqttEvent {
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
            // Straight to `Payload` — one allocation and one copy, where a
            // `Vec` here would be converted again on the way out.
            payload: Payload::from(message.payload),
        })
    }
}

// ===========================================================================
// Data-plane bridges — core's pumps drive these directly. The channels are
// `Sync` (their mutex is `CriticalSectionRawMutex`), so no force-`Send`
// wrapper stands between them and the runner.
// ===========================================================================

/// Outbound sink: turns a `pump_sink` publish into an
/// `AimdbMqttAction::Publish` enqueued onto the session's action channel.
struct MqttSink {
    actions: Arc<ActionChannel>,
}

impl aimdb_core::transport::Connector for MqttSink {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // `qos`/`retain` arrive via the URL query (passed through in
        // `protocol_options`); default to QoS 1 (legacy behaviour), no retain.
        let qos = opt_u8(config, "qos")
            .map(map_qos)
            .unwrap_or(QualityOfService::Qos1);
        let retain = opt_bool(config, "retain").unwrap_or(false);
        let topic = destination.to_string();
        let payload = payload.to_vec();

        Box::pin(async move {
            self.actions
                .send(AimdbMqttAction::Publish {
                    topic,
                    payload,
                    qos,
                    retain,
                })
                .await;
            Ok(())
        })
    }
}

/// Inbound source: drains the session's event channel, yielding each received
/// message as `(topic, payload)` for `pump_source` to fan out.
struct MqttSource {
    events: Arc<EventChannel>,
}

impl aimdb_core::session::Source for MqttSource {
    fn next(&mut self) -> aimdb_core::BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move {
            loop {
                match self.events.receive().await {
                    MqttEvent::ApplicationEvent {
                        event: AimdbMqttEvent::MessageReceived { topic, payload },
                        ..
                    } => return Some((topic, payload)),
                    // Connection lifecycle events carry no record data; skip
                    // and keep draining.
                    _ => continue,
                }
            }
        })
    }
}

/// Force-`Send + Sync` slot for the TLS materials: [`TlsOptions`] holds
/// `&'static mut` exclusive resources (TRNG, record buffers), so it is
/// neither `Sync` nor takeable through the `&self` that
/// [`ConnectorBuilder::build`] receives without interior mutability.
///
/// Core's cell supplies both without `unsafe`: it is `Send + Sync` for any
/// `T: Send`, which is what the `+ Send` on [`TlsOptions`]'s RNG buys.
#[cfg(feature = "embassy-tls")]
pub(crate) type TlsSlot = aimdb_core::session::OneShot<TlsOptions>;

/// Connect and collect the data-plane futures for a plain `mqtt://` session.
pub(crate) fn build_plain<'a, D>(
    db: &'a aimdb_core::builder::AimDb,
    broker_url: &'a str,
    client_id: Option<&'a str>,
    credentials: Option<&'a (String, String)>,
    dialer: &'a D,
) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    Box::pin(async move {
        let topics = inbound_topics(db);
        let broker = parse_broker_url(broker_url)?;
        if broker.tls {
            return Err(build_err("mqtts:// broker URLs require .tls(...)"));
        }
        let connection_settings = static_connection_settings(client_id, credentials);

        let (actions, events, manager_tasks) = setup_manager(
            &broker,
            connection_settings,
            dialer.clone(),
            topics,
            db.runtime_ops(),
        )?;
        Ok(collect_pumps(db, actions, events, manager_tasks))
    })
}

/// Connect and collect the data-plane futures for an `mqtts://` session.
#[cfg(feature = "embassy-tls")]
pub(crate) fn build_tls<'a>(
    db: &'a aimdb_core::builder::AimDb,
    broker_url: &'a str,
    client_id: Option<&'a str>,
    credentials: Option<&'a (String, String)>,
    backend: &'a crate::connector::EmbeddedTls,
) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>> {
    Box::pin(async move {
        let topics = inbound_topics(db);
        let broker = parse_broker_url(broker_url)?;
        if !broker.tls {
            return Err(build_err(".tls(...) requires an mqtts:// broker URL"));
        }
        let options = backend
            .options
            .take()
            .ok_or_else(|| build_err("TLS materials already taken; build() ran twice"))?;
        let connection_settings = static_connection_settings(client_id, credentials);

        let (actions, events, manager_tasks) = setup_tls_manager(
            &broker,
            options,
            connection_settings,
            backend.stack,
            topics,
            db.runtime_ops(),
        )?;
        Ok(collect_pumps(db, actions, events, manager_tasks))
    })
}

/// The inbound topics the session must subscribe on every connection.
fn inbound_topics(db: &aimdb_core::builder::AimDb) -> Vec<String> {
    let inbound_routes = db.collect_inbound_routes("mqtt");
    let topics: Vec<String> = RouterBuilder::from_routes(inbound_routes)
        .build()
        .resource_ids()
        .iter()
        .map(|t| t.to_string())
        .collect();

    #[cfg(feature = "defmt")]
    defmt::info!("MQTT: subscribing to {} inbound topics", topics.len());

    topics
}

/// Outbound publishes and inbound routing ride core's pumps; the session tasks
/// join them.
fn collect_pumps(
    db: &aimdb_core::builder::AimDb,
    actions: Arc<ActionChannel>,
    events: Arc<EventChannel>,
    manager_tasks: Vec<EmbassyBoxFuture>,
) -> Vec<EmbassyBoxFuture> {
    let mut futures = pump_sink(db, "mqtt", Arc::new(MqttSink { actions }));
    futures.extend(pump_source(db, "mqtt", MqttSource { events }));
    futures.extend(manager_tasks);
    futures
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

/// Build the `ConnectionSettings<'static>` for MQTT CONNECT.
///
/// The identity strings are leaked to reach `'static`: one small, bounded leak
/// per connector at build. A shared cell would be smaller but would hand every
/// connector after the first the identity of the first.
fn static_connection_settings(
    client_id: Option<&str>,
    credentials: Option<&(String, String)>,
) -> ConnectionSettings<'static> {
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }

    let client_id = leak(client_id.unwrap_or("aimdb-client"));
    match credentials {
        Some((username, password)) => {
            ConnectionSettings::authenticated(client_id, leak(username), leak(password).as_bytes())
        }
        None => ConnectionSettings::unauthenticated(client_id),
    }
}

/// Set up the plain-TCP broker session loop, returning the action channel
/// (outbound), the event channel (inbound), and the task future. The loop
/// re-subscribes the inbound topics on every connection, so routing survives
/// reconnects. Synchronous — no `.await` — so the caller's `build` future
/// stays `Send`.
fn setup_manager<D>(
    broker: &BrokerUrl,
    connection_settings: ConnectionSettings<'static>,
    dialer: D,
    topics: Vec<String>,
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
) -> Result<ManagerSetup, aimdb_core::DbError>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    Ipv4Addr::from_str(&broker.host).map_err(|_| {
        build_err("Invalid broker IP address (plain mqtt:// needs an IPv4 literal)")
    })?;

    let actions: Arc<ActionChannel> = Arc::new(ActionChannel::new());
    let events: Arc<EventChannel> = Arc::new(EventChannel::new());

    // The transport the session loop dials each cycle, and the clock it runs
    // on — both come from the caller-supplied dialer.
    let delay = dialer.clone();
    let transport =
        crate::embedded::session::SocketTransport::new(dialer, broker.host.clone(), broker.port);

    // SAFETY: every value the session holds is `Send` — `StreamDialer`
    // guarantees `Stream: Send`, the channels are `CriticalSectionRawMutex`
    // and the state cell is a blocking mutex. See `SendSession`.
    let manager_task: EmbassyBoxFuture = Box::pin(unsafe {
        crate::embedded::session::SendSession::new({
            let actions = actions.clone();
            let events = events.clone();
            async move {
                #[cfg(feature = "defmt")]
                defmt::info!("MQTT background task starting");

                crate::embedded::session::run_sessions(
                    transport,
                    topics,
                    connection_settings,
                    Settings::default(),
                    events,
                    actions,
                    delay,
                    runtime,
                )
                .await
            }
        })
    });

    Ok((actions, events, alloc::vec![manager_task]))
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
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
) -> Result<ManagerSetup, aimdb_core::DbError> {
    match host_ip_literal(&broker.host) {
        Some(core::net::IpAddr::V6(_)) => {
            return Err(build_err(
                "mqtts:// with an IPv6 literal can never pass certificate verification — use a hostname",
            ));
        }
        Some(core::net::IpAddr::V4(_)) => {
            // Verifies only via the certificate's CN — private-CA bench
            // setups pin the IP there; public CAs won't issue such certs.
            #[cfg(feature = "defmt")]
            defmt::warn!(
                "MQTT-TLS: broker host is an IP literal; the certificate must carry it in CN — prefer a hostname"
            );
        }
        None => {}
    }
    if options.read_buf.len() < READ_BUF_MIN {
        return Err(build_err(
            "TLS read buffer too small — a TLS 1.3 peer may send 16 KB records; provide at least 16 640 bytes",
        ));
    }

    let actions: Arc<ActionChannel> = Arc::new(ActionChannel::new());
    let events: Arc<EventChannel> = Arc::new(EventChannel::new());

    let network = stack.get();
    let host = broker.host.clone();
    let port = broker.port;
    let sntp_server = options.sntp_server;

    let manager_task = into_box_future({
        let actions = actions.clone();
        let events = events.clone();
        async move {
            #[cfg(feature = "defmt")]
            defmt::info!("MQTT-TLS background task starting");

            #[allow(unreachable_code)]
            {
                let _: () = run_tls(
                    *network,
                    options,
                    host,
                    port,
                    topics,
                    connection_settings,
                    Settings::default(),
                    events,
                    actions,
                    runtime,
                )
                .await;
            }
        }
    });
    let sntp_task = into_box_future(async move {
        #[allow(unreachable_code)]
        {
            let _: () = crate::embedded::sntp::run(*network, sntp_server).await;
        }
    });

    Ok((actions, events, alloc::vec![manager_task, sntp_task]))
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
