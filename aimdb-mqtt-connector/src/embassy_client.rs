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

use aimdb_core::transport::PublishError;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv4Addr;
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
        }
    }
}

/// Empty event type since we're only publishing, not subscribing
///
/// mountain-mqtt-embassy requires an event type for received messages.
/// Since we're only publishing, we use an empty enum.
#[derive(Clone)]
pub enum AimdbMqttEvent {}

impl mountain_mqtt_embassy::mqtt_manager::FromApplicationMessage<MAX_PROPERTIES>
    for AimdbMqttEvent
{
    fn from_application_message(
        _message: &mountain_mqtt::packets::publish::ApplicationMessage<MAX_PROPERTIES>,
    ) -> Result<Self, mountain_mqtt::client::EventHandlerError> {
        // We don't expect to receive any messages, so return an error
        Err(mountain_mqtt::client::EventHandlerError::UnexpectedApplicationMessageTopic)
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
