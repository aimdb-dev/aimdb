//! Per-session broker state, the event handler that feeds the event channel,
//! and the pump that keeps one connection alive.
//!
//! Channels use `CriticalSectionRawMutex`, so they are `Sync` and the sink and
//! source are plain `Connector`/`Source` impls with no force-`Send` wrapper.
//! Time comes from core's [`Delay`] and the runtime's monotonic clock, so the
//! pump names no executor.

use core::cell::RefCell;
use core::time::Duration;

use aimdb_core::session::Delay;
use aimdb_core::RuntimeOps;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Channel;
use mountain_mqtt::client::{
    Client, ClientError, ClientReceivedEvent, ConnectionSettings, EventHandler, EventHandlerError,
};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::mqtt_manager::{ConnectionId, MqttOperations};
use mountain_mqtt::packets::publish::ApplicationMessage;

/// The event channel: broker session to `pump_source`.
pub(crate) type EventChannel<E, const Q: usize> = Channel<CriticalSectionRawMutex, MqttEvent<E>, Q>;

/// The action channel: `pump_sink` to broker session.
pub(crate) type ActionChannel<A, const Q: usize> = Channel<CriticalSectionRawMutex, A, Q>;

/// Monotonic milliseconds. Only differences are meaningful.
pub(crate) fn now_ms(runtime: &dyn RuntimeOps) -> u64 {
    runtime.now_nanos() / 1_000_000
}

/// Convert a received [`ApplicationMessage`] into an application event.
pub trait FromApplicationMessage<const P: usize>: Sized {
    /// Build the event, or reject the message.
    fn from_application_message(message: &ApplicationMessage<P>)
        -> Result<Self, EventHandlerError>;
}

/// Why a session ended.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Error {
    /// The MQTT client reported an error.
    Client(ClientError),
    /// No acknowledgement arrived within `connection_event_max_interval`.
    MqttServerUnresponsive,
}

impl From<ClientError> for Error {
    fn from(value: ClientError) -> Self {
        Self::Client(value)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Error {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Error::Client(e) => defmt::write!(f, "Client({})", e),
            Error::MqttServerUnresponsive => defmt::write!(f, "MqttServerUnresponsive"),
        }
    }
}

/// Session cadence: how often to ping, how long to wait, when to give up.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Minimum interval between pings.
    pub ping_interval: Duration,
    /// Maximum silence from the broker before the session is declared dead.
    pub connection_event_max_interval: Duration,
    /// Wait between a failed session and the next dial.
    pub reconnection_delay: Duration,
    /// Delay applied to each pump iteration.
    pub poll_interval: Duration,
    /// Maximum round-trip wait for a packet that expects a response.
    pub response_timeout: Duration,
    /// How long a connection must hold before it counts as stable.
    pub stabilisation_interval: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_millis(2_000),
            connection_event_max_interval: Duration::from_millis(10_000),
            reconnection_delay: Duration::from_millis(2_000),
            poll_interval: Duration::from_millis(10),
            response_timeout: Duration::from_millis(5_000),
            stabilisation_interval: Duration::from_millis(5_000),
        }
    }
}

/// What the session reports to the event channel.
#[derive(Debug, Clone)]
pub enum MqttEvent<E> {
    /// An application message arrived and converted to `E`.
    ApplicationEvent {
        /// The connection it arrived on.
        connection_id: ConnectionId,
        /// The converted message.
        event: E,
    },
    /// A new connection was established.
    Connected {
        /// The new connection.
        connection_id: ConnectionId,
    },
    /// A connection held for `stabilisation_interval`.
    ConnectionStable {
        /// The connection that stabilised.
        connection_id: ConnectionId,
    },
    /// A connection ended; the next one is dialled automatically.
    Disconnected {
        /// The connection that ended.
        connection_id: ConnectionId,
        /// Why it ended.
        error: Error,
    },
    /// A subscription was granted below the QoS requested.
    SubscriptionGrantedBelowMaximumQos {
        /// The connection it was granted on.
        connection_id: ConnectionId,
        /// What the broker granted.
        granted_qos: QualityOfService,
        /// What was asked for.
        maximum_qos: QualityOfService,
    },
    /// A published message reached no subscriber.
    PublishedMessageHadNoMatchingSubscribers {
        /// The connection it was published on.
        connection_id: ConnectionId,
    },
    /// An unsubscribe named a subscription the broker did not hold.
    NoSubscriptionExisted {
        /// The connection it was sent on.
        connection_id: ConnectionId,
    },
}

/// Per-connection bookkeeping, shared between the pump and its event handler.
///
/// The blocking mutex is what makes `&SessionState` `Send`: a bare `RefCell`
/// is not `Sync`, so a session future holding one could not be boxed as the
/// runner requires. Every lock is a straight-line read or write, never held
/// across an `await`.
pub(crate) struct SessionState {
    inner: BlockingMutex<CriticalSectionRawMutex, RefCell<Inner>>,
}

struct Inner {
    /// When the broker last proved it was alive.
    last_connection_event_ms: u64,
}

impl SessionState {
    /// Fresh state for a new connection; the liveness window starts now.
    pub(crate) fn new(now_ms: u64) -> Self {
        Self {
            inner: BlockingMutex::new(RefCell::new(Inner {
                last_connection_event_ms: now_ms,
            })),
        }
    }

    fn record_connection_event(&self, now_ms: u64) {
        self.inner
            .lock(|state| state.borrow_mut().last_connection_event_ms = now_ms);
    }

    fn last_connection_event_ms(&self) -> u64 {
        self.inner
            .lock(|state| state.borrow().last_connection_event_ms)
    }
}

/// Forwards received MQTT events onto the event channel and refreshes the
/// liveness timestamp on every broker acknowledgement.
pub(crate) struct ChannelEventHandler<'a, E, const P: usize, const Q: usize>
where
    E: FromApplicationMessage<P> + Clone,
{
    connection_id: ConnectionId,
    events: &'a EventChannel<E, Q>,
    state: &'a SessionState,
    runtime: &'a dyn RuntimeOps,
}

impl<'a, E, const P: usize, const Q: usize> ChannelEventHandler<'a, E, P, Q>
where
    E: FromApplicationMessage<P> + Clone,
{
    pub(crate) fn new(
        connection_id: ConnectionId,
        events: &'a EventChannel<E, Q>,
        state: &'a SessionState,
        runtime: &'a dyn RuntimeOps,
    ) -> Self {
        Self {
            connection_id,
            events,
            state,
            runtime,
        }
    }
}

impl<E, const P: usize, const Q: usize> EventHandler<P> for ChannelEventHandler<'_, E, P, Q>
where
    E: FromApplicationMessage<P> + Clone,
{
    async fn handle_event(
        &mut self,
        event: ClientReceivedEvent<'_, P>,
    ) -> Result<(), EventHandlerError> {
        let connection_id = self.connection_id;
        match event {
            ClientReceivedEvent::ApplicationMessage(message) => {
                let event = E::from_application_message(&message)?;
                self.events
                    .send(MqttEvent::ApplicationEvent {
                        connection_id,
                        event,
                    })
                    .await;
            }
            ClientReceivedEvent::Ack => {
                self.state.record_connection_event(now_ms(self.runtime));
            }
            ClientReceivedEvent::SubscriptionGrantedBelowMaximumQos {
                granted_qos,
                maximum_qos,
            } => {
                self.events
                    .send(MqttEvent::SubscriptionGrantedBelowMaximumQos {
                        connection_id,
                        granted_qos,
                        maximum_qos,
                    })
                    .await
            }
            ClientReceivedEvent::PublishedMessageHadNoMatchingSubscribers => {
                self.events
                    .send(MqttEvent::PublishedMessageHadNoMatchingSubscribers { connection_id })
                    .await
            }
            ClientReceivedEvent::NoSubscriptionExisted => {
                self.events
                    .send(MqttEvent::NoSubscriptionExisted { connection_id })
                    .await
            }
        }
        Ok(())
    }
}

/// Drive one MQTT session until an error ends it: connect, subscribe
/// `subscribe_topics`, then keep it alive while dispatching actions and
/// forwarding events.
///
/// `subscribe_topics` is re-sent on every call, i.e. once per connection, so
/// inbound routing survives a reconnect.
///
/// # Delivery
///
/// **At most once, at this layer.** An action is taken off `actions` before it
/// is performed, so the one action in flight when the session ends is lost;
/// everything still queued survives, because `actions` outlives the session.
/// The action logs what it dropped before the error propagates.
///
/// Resending is deliberately not done here. The window is narrow — a dead link
/// is normally found by the 10 ms poll or the 2 s ping, not by a publish — and
/// the case where a publish *is* the detector is a response timeout, where the
/// broker has most likely already received the message and a resend would
/// duplicate it. This layer cannot tell a telemetry sample (resend is
/// pointless, a fresher value is already queued behind it) from a command
/// (resend may be actively wrong). An application that needs at-least-once
/// knows which it has, and can re-produce on [`MqttEvent::Connected`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_messages<'a, A, C, E, D, const P: usize, const Q: usize>(
    connection_id: ConnectionId,
    client: &mut C,
    state: &SessionState,
    connection_settings: &ConnectionSettings<'static>,
    subscribe_topics: &[(&str, QualityOfService)],
    events: &EventChannel<E, Q>,
    actions: &ActionChannel<A, Q>,
    settings: &Settings,
    delay: &D,
    runtime: &dyn RuntimeOps,
) -> Result<(), Error>
where
    C: Client<'a>,
    A: MqttOperations + Clone,
    E: FromApplicationMessage<P> + Clone,
    D: Delay,
{
    client.connect(connection_settings).await?;
    events.send(MqttEvent::Connected { connection_id }).await;

    for (topic, qos) in subscribe_topics {
        client.subscribe(topic, *qos).await?;
    }

    let ping_interval = settings.ping_interval.as_millis() as u64;
    let stabilisation_interval = settings.stabilisation_interval.as_millis() as u64;
    let max_silence = settings.connection_event_max_interval.as_millis() as u64;

    let mut connected_at = Some(now_ms(runtime));
    let mut last_ping_ms = now_ms(runtime);

    loop {
        delay.sleep(settings.poll_interval).await;
        let now = now_ms(runtime);

        if now.saturating_sub(last_ping_ms) > ping_interval {
            last_ping_ms = now;
            client.send_ping().await?;
        }

        if let Some(since) = connected_at {
            if now.saturating_sub(since) > stabilisation_interval {
                connected_at = None;
                events
                    .send(MqttEvent::ConnectionStable { connection_id })
                    .await;
            }
        }

        if now.saturating_sub(state.last_connection_event_ms()) > max_silence {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT: broker unresponsive");
            return Err(Error::MqttServerUnresponsive);
        }

        // Poll with no delay while packets are waiting.
        while client.poll(false).await? {}

        // A failed action ends the session, and the action is gone with it —
        // see this function's "Delivery" note. `is_retry` is always `false`:
        // nothing is ever performed twice.
        while let Ok(mut action) = actions.try_receive() {
            action
                .perform(
                    client,
                    connection_settings.client_id(),
                    connection_id,
                    false,
                )
                .await?;
        }
    }
}
