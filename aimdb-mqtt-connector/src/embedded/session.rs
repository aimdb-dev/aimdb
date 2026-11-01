//! The broker session loop for the [`Embedded`](crate::connector::Embedded)
//! backend: dial, run one MQTT session over the stream's two halves, wait,
//! repeat.
//!
//! Built on core's [`ByteStream`](aimdb_core::session::ByteStream) alone: any
//! runtime that can dial a
//! [`StreamDialer`](aimdb_core::session::StreamDialer) can speak MQTT.

use crate::events::{report, MqttEvent};

use core::future::Future;

/// Asserts that a broker session future is `Send`.
///
/// Everything the session holds is `Send` — [`StreamDialer`] guarantees
/// `Stream: Send`, the channels use `CriticalSectionRawMutex` — but
/// `embedded-io-async` puts no `Send` bound on its futures, and naming that
/// bound through a generic transport needs return-type notation, unstable on
/// the pinned toolchain.
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
/// The dialer supplies both the stream and the clock. `topics` is re-subscribed
/// on each connection, so inbound routing survives a reconnect.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sessions<D>(
    dialer: D,
    host: alloc::string::String,
    port: u16,
    topics: alloc::vec::Vec<crate::subscribe_qos::Subscription>,
    connection_settings: mountain_mqtt::client::ConnectionSettings<'static>,
    settings: crate::embedded::manager::Settings,
    inbound: aimdb_core::InboundDispatch,
    mut outbound: aimdb_core::OutboundRoutes,
    opts: alloc::vec::Vec<crate::publish_opts::PublishOpts>,
    write_buffer: usize,
    clock: alloc::sync::Arc<dyn aimdb_core::InstanceClock>,
) -> !
where
    D: aimdb_core::session::StreamDialer + aimdb_core::session::Delay,
{
    use aimdb_core::session::{ByteStream, Delay};
    use mountain_mqtt::data::quality_of_service::QualityOfService;

    use crate::embedded::session_loop::run_session;
    use crate::embedded::write_ring::WriteRing;

    // Built once and borrowed for the loop; re-sent on every connection.
    let subscribe_topics: alloc::vec::Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|s| (s.filter.as_str(), crate::embedded::mountain_qos(s.qos)))
        .collect();

    // Allocated once and reused by every session.
    let ring = WriteRing::new(write_buffer);

    loop {
        let mut stream = match dialer.connect(&host, port).await {
            Ok(stream) => stream,
            Err(_e) => {
                report(MqttEvent::ConnectFailed);
                Delay::sleep(&dialer, settings.reconnection_delay).await;
                continue;
            }
        };

        // The halves live exactly as long as the session that reads and writes
        // them, which is why borrowed halves are enough.
        let (rx, tx) = stream.split();
        let error = run_session(
            rx,
            tx,
            &connection_settings,
            &subscribe_topics,
            &inbound,
            &mut outbound,
            &opts,
            &ring,
            &settings,
            &dialer,
            clock.as_ref(),
        )
        .await;

        report(MqttEvent::SessionFailed { error });

        Delay::sleep(&dialer, settings.reconnection_delay).await;
    }
}
