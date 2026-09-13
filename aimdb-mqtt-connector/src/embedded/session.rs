//! The broker session loop for the [`Embedded`](crate::connector::Embedded)
//! backend: dial, run one MQTT session over the stream's two halves, wait,
//! repeat.
//!
//! Built on core's [`ByteStream`](aimdb_core::session::ByteStream) alone. The
//! MQTT client used to need `receive_if_ready` — a non-blocking peek a byte
//! stream cannot express and a TLS session cannot honestly provide — because
//! the session polled. Nothing polls any more (design 053), so the peek is
//! gone and with it the transport seam that existed to carry it: a runtime that
//! can dial a [`StreamDialer`](aimdb_core::session::StreamDialer) can speak
//! MQTT, with no protocol code and no `embedded-io-async` of its own.

use core::future::Future;

/// Bridges core's [`Delay`](aimdb_core::session::Delay) to the `DelayNs` the
/// MQTT client wants, so the client's timeouts run on the adapter's clock.
///
/// Only the TLS path still needs it; it goes when TLS joins the same session
/// loop as the plain path.
#[cfg(feature = "embedded-tls")]
pub(crate) struct ClientDelay<'a, D>(pub(crate) &'a D);

#[cfg(feature = "embedded-tls")]
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

/// Dial, run one session, wait, repeat. Never returns.
///
/// One implementation for every transport: the dialer supplies both the stream
/// and the clock. `topics` is re-subscribed on each connection, so inbound
/// routing survives a reconnect.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sessions<D>(
    dialer: D,
    host: alloc::string::String,
    port: u16,
    topics: alloc::vec::Vec<alloc::string::String>,
    connection_settings: mountain_mqtt::client::ConnectionSettings<'static>,
    settings: crate::embedded::manager::Settings,
    events: alloc::sync::Arc<crate::embedded::EventChannel>,
    actions: alloc::sync::Arc<crate::embedded::ActionChannel>,
    runtime: alloc::sync::Arc<dyn aimdb_core::RuntimeOps>,
) -> !
where
    D: aimdb_core::session::StreamDialer + aimdb_core::session::Delay,
{
    use aimdb_core::session::{ByteStream, Delay};
    use mountain_mqtt::data::quality_of_service::QualityOfService;
    use mountain_mqtt::mqtt_manager::ConnectionId;

    use crate::embedded::manager::MqttEvent;
    use crate::embedded::session_loop::run_session;

    // Built once and borrowed for the loop; re-sent on every connection.
    let subscribe_topics: alloc::vec::Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|topic| (topic.as_str(), QualityOfService::Qos1))
        .collect();

    let mut connection_index = 0u32;

    loop {
        let mut stream = match dialer.connect(&host, port).await {
            Ok(stream) => stream,
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT: connect failed, will retry");
                Delay::sleep(&dialer, settings.reconnection_delay).await;
                continue;
            }
        };

        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        // The halves live exactly as long as the session that reads and writes
        // them, which is why borrowed halves are enough (design 053 §6.1).
        let (rx, tx) = stream.split();
        let error = run_session(
            connection_id,
            rx,
            tx,
            &connection_settings,
            &subscribe_topics,
            &events,
            &actions,
            &settings,
            &dialer,
            runtime.as_ref(),
        )
        .await;

        #[cfg(feature = "defmt")]
        defmt::warn!("MQTT: session errored: {:?}", error);
        events
            .send(MqttEvent::Disconnected {
                connection_id,
                error,
            })
            .await;

        Delay::sleep(&dialer, settings.reconnection_delay).await;
    }
}
