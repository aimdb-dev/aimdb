//! The broker transport seam for the [`Embedded`](crate::connector::Embedded)
//! backend.
//!
//! Built on `mountain-mqtt`'s own [`Connection`] rather than core's
//! [`ByteStream`](aimdb_core::session::ByteStream): the MQTT client needs
//! `receive_if_ready` — a non-blocking peek — which a byte stream does not
//! express and a TLS session cannot provide (its readiness is two-layered;
//! see the `embassy_tls` module). Wrapping core's trait would mean every
//! TLS-like transport faking a capability, so the client's own seam is the
//! honest one.
//!
//! A new runtime supplies MQTT by implementing this once. Anything offering
//! `embedded_io_async::{Read, Write}` plus `ReadReady` — an lwIP socket, say —
//! gets there through `mountain_mqtt::embedded_io_async::ConnectionEmbedded`
//! with no protocol code to touch.

use aimdb_core::session::TransportResult;
use core::future::Future;
use mountain_mqtt::packet_client::Connection;

/// Opens one broker connection per session.
///
/// The connector calls this once per reconnect cycle, so an implementation
/// must be able to produce a fresh connection each time.
pub trait BrokerTransport {
    /// The connection this transport produces.
    type Connection: Connection;

    /// Open a connection to the broker.
    fn connect(&self) -> impl Future<Output = TransportResult<Self::Connection>> + Send;
}

/// Bridges core's [`StreamDialer`](aimdb_core::session::StreamDialer) to
/// [`BrokerTransport`] for any adapter whose stream also offers the
/// `embedded-io-async` trio.
///
/// This is the path a new runtime takes: implement `StreamDialer` and delegate
/// `Read`/`Write`/`ReadReady` on the stream, and MQTT follows with no code
/// here. TLS does not come this way — its readiness is two-layered, so it
/// implements [`BrokerTransport`] directly.
pub struct SocketTransport<D> {
    dialer: D,
    host: alloc::string::String,
    port: u16,
}

impl<D> SocketTransport<D> {
    /// Dial `host:port` through `dialer` for each broker session.
    pub fn new(dialer: D, host: impl Into<alloc::string::String>, port: u16) -> Self {
        Self {
            dialer,
            host: host.into(),
            port,
        }
    }
}

impl<D> BrokerTransport for SocketTransport<D>
where
    D: aimdb_core::session::StreamDialer + Sync,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    type Connection = mountain_mqtt::embedded_io_async::ConnectionEmbedded<D::Stream>;

    async fn connect(&self) -> TransportResult<Self::Connection> {
        let stream = self.dialer.connect(&self.host, self.port).await?;
        Ok(mountain_mqtt::embedded_io_async::ConnectionEmbedded::new(
            stream,
        ))
    }
}

/// The broker session loop: connect, run MQTT until the session ends, wait,
/// repeat. Never returns.
///
/// One implementation for every transport. `handle_messages` re-subscribes
/// `subscribe_topics` on each connection, so inbound routing survives a
/// reconnect — the property `run_with_subscriptions` used to provide, now
/// explicit here because injecting a transport means giving that helper up.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sessions<T>(
    transport: T,
    topics: alloc::vec::Vec<alloc::string::String>,
    connection_settings: mountain_mqtt::client::ConnectionSettings<'static>,
    settings: mountain_mqtt_embassy::mqtt_manager::Settings,
    event_sender: crate::embassy_client::EventSender,
    mut action_receiver: crate::embassy_client::ActionReceiver,
) -> !
where
    T: BrokerTransport,
{
    use core::cell::RefCell;
    use mountain_mqtt::client::ClientNoQueue;
    use mountain_mqtt::data::quality_of_service::QualityOfService;
    use mountain_mqtt::mqtt_manager::ConnectionId;
    use mountain_mqtt_embassy::mqtt_manager::{
        handle_messages, ChannelEventHandler, MqttEvent, State,
    };

    // Built once and borrowed for the loop; re-sent on every connection.
    let subscribe_topics: alloc::vec::Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|topic| (topic.as_str(), QualityOfService::Qos1))
        .collect();

    let mut mqtt_buffer = [0u8; crate::embassy_client::BUFFER_SIZE];
    let mut connection_index = 0u32;

    loop {
        let connection = match transport.connect().await {
            Ok(connection) => connection,
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT: connect failed, will retry");
                embassy_time::Timer::after(settings.reconnection_delay).await;
                continue;
            }
        };

        let state: RefCell<State<crate::embassy_client::AimdbMqttAction>> =
            RefCell::new(State::new());
        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        let event_handler = ChannelEventHandler::new(connection_id, &event_sender, &state);
        let mut client = ClientNoQueue::new(
            connection,
            &mut mqtt_buffer,
            mountain_mqtt::embedded_hal_async::DelayEmbedded::new(embassy_time::Delay),
            settings.response_timeout.as_millis() as u32,
            event_handler,
        );

        if let Err(error) = handle_messages(
            connection_id,
            &mut client,
            &state,
            &connection_settings,
            &subscribe_topics,
            &event_sender,
            &mut action_receiver,
            &settings,
        )
        .await
        {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT: session errored: {:?}", error);
            event_sender
                .send(MqttEvent::Disconnected {
                    connection_id,
                    error,
                })
                .await;
        }

        embassy_time::Timer::after(settings.reconnection_delay).await;
    }
}
