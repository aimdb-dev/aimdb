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

/// Bridges core's [`Delay`](aimdb_core::session::Delay) to the `DelayNs` the
/// MQTT client wants, so the client's timeouts run on the adapter's clock.
pub(crate) struct ClientDelay<'a, D>(pub(crate) &'a D);

impl<D> embedded_hal_async::delay::DelayNs for ClientDelay<'_, D>
where
    D: aimdb_core::session::Delay,
{
    async fn delay_ns(&mut self, ns: u32) {
        self.0
            .sleep(core::time::Duration::from_nanos(u64::from(ns)))
            .await
    }
}

/// Asserts that a broker session future is `Send`.
///
/// Everything the session holds is `Send`: [`StreamDialer`] guarantees
/// `Stream: Send`, the channels use `CriticalSectionRawMutex`, and the state
/// cell is a blocking mutex. What the compiler cannot see through is
/// `embedded-io-async` — its traits put no `Send` bound on their futures, and
/// the loop reaches them through a generic transport, so naming the bound needs
/// return-type notation, still unstable on the pinned toolchain.
///
/// This is weaker than an executor assumption, not stronger: it rests on a
/// trait guarantee, so it holds under a preemptive scheduler too.
pub(crate) struct SendSession<F>(F);

// SAFETY: upheld by the caller of `SendSession::new`.
unsafe impl<F> Send for SendSession<F> {}

impl<F> SendSession<F> {
    /// # Safety
    ///
    /// Every value `f` holds across a suspend point must actually be `Send`.
    pub(crate) unsafe fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F: Future> Future for SendSession<F> {
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<F::Output> {
        // SAFETY: a transparent projection; `SendSession` is never moved out of.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

/// The broker session loop: connect, run MQTT until the session ends, wait,
/// repeat. Never returns.
///
/// One implementation for every transport. The manager re-subscribes
/// `topics` on each connection, so inbound routing survives a reconnect.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sessions<T, D>(
    transport: T,
    topics: alloc::vec::Vec<alloc::string::String>,
    connection_settings: mountain_mqtt::client::ConnectionSettings<'static>,
    settings: crate::embedded::manager::Settings,
    events: alloc::sync::Arc<crate::embedded::EventChannel>,
    actions: alloc::sync::Arc<crate::embedded::ActionChannel>,
    delay: D,
    runtime: alloc::sync::Arc<dyn aimdb_core::RuntimeOps>,
) -> !
where
    T: BrokerTransport,
    D: aimdb_core::session::Delay,
{
    use mountain_mqtt::client::ClientNoQueue;
    use mountain_mqtt::data::quality_of_service::QualityOfService;
    use mountain_mqtt::mqtt_manager::ConnectionId;

    use crate::embedded::manager::{
        handle_messages, now_ms, ChannelEventHandler, MqttEvent, SessionState,
    };

    // Built once and borrowed for the loop; re-sent on every connection.
    let subscribe_topics: alloc::vec::Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|topic| (topic.as_str(), QualityOfService::Qos1))
        .collect();

    let mut mqtt_buffer = [0u8; crate::embedded::BUFFER_SIZE];
    let mut connection_index = 0u32;

    loop {
        let connection = match transport.connect().await {
            Ok(connection) => connection,
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT: connect failed, will retry");
                delay.sleep(settings.reconnection_delay).await;
                continue;
            }
        };

        let state = SessionState::new(now_ms(runtime.as_ref()));
        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        let event_handler =
            ChannelEventHandler::new(connection_id, &events, &state, runtime.as_ref());
        let mut client = ClientNoQueue::new(
            connection,
            &mut mqtt_buffer,
            mountain_mqtt::embedded_hal_async::DelayEmbedded::new(ClientDelay(&delay)),
            settings.response_timeout.as_millis() as u32,
            event_handler,
        );

        if let Err(error) = handle_messages(
            connection_id,
            &mut client,
            &state,
            &connection_settings,
            &subscribe_topics,
            &events,
            &actions,
            &settings,
            &delay,
            runtime.as_ref(),
        )
        .await
        {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT: session errored: {:?}", error);
            events
                .send(MqttEvent::Disconnected {
                    connection_id,
                    error,
                })
                .await;
        }

        delay.sleep(settings.reconnection_delay).await;
    }
}
