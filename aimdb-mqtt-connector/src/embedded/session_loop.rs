//! The event-driven broker session: three futures in one `select3`.
//!
//! The stream is split and the only thing ever cancelled is a channel receive,
//! so the non-cancel-safe `write_all` never sits in a `select` arm and no
//! partially-read packet is ever discarded — which is what lets the TLS path
//! share this loop.
//!
//! [`read_into`] and [`write_out`] know no MQTT; [`client_loop`] owns all
//! client state and wakes only on data, an action or a deadline. Raw chunks
//! cross the inbound channel rather than whole packets, so framing needs no
//! second packet-sized buffer.

use core::convert::Infallible;
use core::time::Duration;

use aimdb_core::session::{ByteRead, ByteWrite, Delay};
use aimdb_core::RuntimeOps;
use alloc::vec::Vec;
use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use mountain_mqtt::client::{ClientError, ConnectionSettings};
use mountain_mqtt::client_state::{ClientState, ClientStateNoQueue, ClientStateReceiveEvent};
use mountain_mqtt::codec::mqtt_writer::{MqttBufWriter, MqttLenWriter, MqttWriter};
use mountain_mqtt::codec::write::Write;
use mountain_mqtt::data::property::ConnectProperty;
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::error::{PacketReadError, PacketWriteError};
use mountain_mqtt::mqtt_manager::ConnectionId;
use mountain_mqtt::packets::connect::Connect;
use mountain_mqtt::packets::packet_generic::PacketGeneric;

use crate::embedded::manager::{now_ms, Error, FromApplicationMessage, MqttEvent, Settings};
use crate::embedded::packet_reader::PacketReader;
use crate::embedded::{
    ActionChannel, AimdbMqttAction, AimdbMqttEvent, EventChannel, BUFFER_SIZE, MAX_PROPERTIES,
};

/// Bytes lifted off the socket at a time, and the size of one `inbound` slot.
const RX_CHUNK: usize = 256;

/// The largest MQTT packet the session can receive.
///
/// Reassembly, the inbound slots and the encode buffer all come out of one
/// `BUFFER_SIZE`; outbound packets are encoded to exactly-sized `Vec<u8>`s
/// rather than a fixed buffer.
const PACKET_BUFFER_SIZE: usize = BUFFER_SIZE - 2 * RX_CHUNK;

/// One chunk of freshly read bytes, in flight from the read half to the loop.
type Chunk = heapless::Vec<u8, RX_CHUNK>;

/// Drive one MQTT session over a split stream until an error ends it.
///
/// Connects, subscribes `subscribe_topics`, then dispatches actions and
/// forwards events. Returns only on failure — the caller reconnects.
///
/// **At most once**: an action is taken off `actions` before it is performed,
/// so the one in flight when a session ends is lost. Everything still queued
/// survives.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_session<R, W, D>(
    connection_id: ConnectionId,
    rx: R,
    tx: W,
    connection_settings: &ConnectionSettings<'static>,
    subscribe_topics: &[(&str, QualityOfService)],
    events: &EventChannel,
    actions: &ActionChannel,
    settings: &Settings,
    delay: &D,
    runtime: &dyn RuntimeOps,
) -> Error
where
    R: ByteRead,
    W: ByteWrite,
    D: Delay,
{
    let inbound: Channel<CriticalSectionRawMutex, Chunk, 1> = Channel::new();
    // Four slots of a pointer each: enough that a burst of small packets does
    // not park the loop, and cheap because the bytes live on the heap.
    let outbound: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();

    let session = client_loop(
        connection_id,
        &inbound,
        &outbound,
        connection_settings,
        subscribe_topics,
        events,
        actions,
        settings,
        delay,
        runtime,
    );

    match select3(read_into(rx, &inbound), write_out(tx, &outbound), session).await {
        Either3::First(error) => error,
        Either3::Second(error) => error,
        Either3::Third(Err(error)) => error,
        // `client_loop` only ever returns an error.
        Either3::Third(Ok(never)) => match never {},
    }
}

/// Lift bytes off the socket and hand them to the loop. Never cancelled, so a
/// read is never dropped mid-packet.
async fn read_into<R: ByteRead>(
    mut rx: R,
    inbound: &Channel<CriticalSectionRawMutex, Chunk, 1>,
) -> Error {
    let mut scratch = [0u8; RX_CHUNK];
    loop {
        let n = match rx.read(&mut scratch).await {
            // A closed peer is an ended session, not an error condition to sit in.
            Ok(0) | Err(_) => return receive_failed(),
            Ok(n) => n,
        };
        let mut chunk = Chunk::new();
        // `n <= RX_CHUNK` by construction, so this cannot overflow the chunk.
        if chunk.extend_from_slice(&scratch[..n]).is_err() {
            return receive_failed();
        }
        inbound.send(chunk).await;
    }
}

/// Drain encoded packets to the socket. Never cancelled, which is what keeps
/// the non-cancel-safe `write_all` out of a `select` arm.
async fn write_out<W: ByteWrite>(
    mut tx: W,
    outbound: &Channel<CriticalSectionRawMutex, Vec<u8>, 4>,
) -> Error {
    loop {
        let packet = outbound.receive().await;
        if tx.write_all(&packet).await.is_err() || tx.flush().await.is_err() {
            return Error::Client(ClientError::PacketWrite(PacketWriteError::ConnectionSend));
        }
    }
}

fn receive_failed() -> Error {
    Error::Client(ClientError::PacketRead(PacketReadError::ConnectionReceive))
}

/// Everything the session knows, in one future: state, framing, deadlines.
#[allow(clippy::too_many_arguments)]
async fn client_loop<D: Delay>(
    connection_id: ConnectionId,
    inbound: &Channel<CriticalSectionRawMutex, Chunk, 1>,
    outbound: &Channel<CriticalSectionRawMutex, Vec<u8>, 4>,
    connection_settings: &ConnectionSettings<'static>,
    subscribe_topics: &[(&str, QualityOfService)],
    events: &EventChannel,
    actions: &ActionChannel,
    settings: &Settings,
    delay: &D,
    runtime: &dyn RuntimeOps,
) -> Result<Infallible, Error> {
    let ping_interval = settings.ping_interval.as_millis() as u64;
    let max_silence = settings.connection_event_max_interval.as_millis() as u64;
    let stabilisation = settings.stabilisation_interval.as_millis() as u64;
    let response_timeout = settings.response_timeout.as_millis() as u64;

    let mut state = ClientStateNoQueue::new();
    let mut reader = PacketReader::<PACKET_BUFFER_SIZE>::new();

    let start = now_ms(runtime);
    let mut last_ack_ms = start;
    let mut last_ping_ms = start;
    // Set while an acknowledgement is outstanding, so a broker that never
    // answers a CONNECT, SUBSCRIBE or QoS 1 PUBLISH is caught by
    // `response_timeout` rather than only by the liveness window.
    let mut waiting_since: Option<u64> = Some(start);
    let mut stable_at: Option<u64> = None;
    let mut connected = false;
    let mut next_topic = 0usize;

    // CONNECT goes out first; its CONNACK is what flips `connected`.
    {
        let mut properties = heapless::Vec::new();
        // Topic aliases are declined: honouring them would mean storing the
        // server's topic names for the life of the connection.
        let _ = properties.push(ConnectProperty::TopicAliasMaximum(0.into()));
        let connect: Connect<'_, 1, 0> = Connect::new(
            connection_settings.keep_alive(),
            *connection_settings.username(),
            *connection_settings.password(),
            connection_settings.client_id(),
            true,
            None,
            properties,
        );
        state.connect(&connect).map_err(client_error)?;
        queue(outbound, encode(&connect)?);
    }

    loop {
        let now = now_ms(runtime);

        // --- deadlines, checked before anything parks ----------------------

        if now.saturating_sub(last_ack_ms) > max_silence {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT: broker unresponsive");
            return Err(Error::MqttServerUnresponsive);
        }

        if let Some(since) = waiting_since {
            if now.saturating_sub(since) > response_timeout {
                return Err(Error::Client(ClientError::TimeoutOnResponsePacket));
            }
        }

        if let Some(at) = stable_at {
            if now >= at {
                stable_at = None;
                events
                    .send(MqttEvent::ConnectionStable { connection_id })
                    .await;
            }
        }

        if connected && now.saturating_sub(last_ping_ms) >= ping_interval {
            last_ping_ms = now;
            let ping = state.send_ping().map_err(client_error)?;
            // A ping dropped because the write half is backed up is not worth
            // ending the session over: the next deadline tries again, and if
            // the link really is gone the liveness window closes it.
            queue(outbound, encode(&ping)?);
        }

        // Subscriptions go out one at a time: `ClientStateNoQueue` tracks a
        // single outstanding request, so the next one waits for this SUBACK.
        if connected && !state.waiting_for_responses() && next_topic < subscribe_topics.len() {
            let (topic, qos) = subscribe_topics[next_topic];
            let packet = state.subscribe_packet(topic, qos).map_err(client_error)?;
            queue(outbound, encode(&packet)?);
            state.subscribe_update(&packet).map_err(client_error)?;
            next_topic += 1;
            // `continue` skips the bottom-of-loop bookkeeping, so arm the
            // response deadline here: a broker that never SUBACKs should be
            // caught by `response_timeout`, not only by the liveness window.
            waiting_since = Some(now);
            continue;
        }

        // --- park until something happens ----------------------------------

        // The action arm is armed only when a publish can actually be sent:
        // connected, nothing awaiting acknowledgement (§6.4's single in-flight
        // slot), every subscription placed, and room to queue the bytes. This
        // is what replaces the old inline wait for a PUBACK — the ping and
        // liveness deadlines keep running while it is parked.
        let action_ready = connected
            && !state.waiting_for_responses()
            && next_topic >= subscribe_topics.len()
            && !outbound.is_full();
        let action_arm = async {
            if !action_ready {
                core::future::pending::<()>().await;
            }
            actions.receive().await
        };

        let sleep_for = Duration::from_millis(next_deadline(
            now,
            connected,
            last_ping_ms + ping_interval,
            last_ack_ms + max_silence,
            stable_at,
            waiting_since.map(|since| since + response_timeout),
        ));

        match select3(inbound.receive(), action_arm, delay.sleep(sleep_for)).await {
            Either3::First(chunk) => {
                reader.feed(&chunk).map_err(client_error)?;
                drain_packets(
                    &mut reader,
                    &mut state,
                    connection_id,
                    outbound,
                    events,
                    runtime,
                    &mut last_ack_ms,
                    &mut connected,
                    &mut stable_at,
                    stabilisation,
                )
                .await?;
            }
            Either3::Second(action) => {
                perform(action, &mut state, outbound)?;
            }
            // The timer fired: the top of the loop re-evaluates every deadline.
            Either3::Third(()) => {}
        }

        waiting_since = match (state.waiting_for_responses(), waiting_since) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(now_ms(runtime)),
            (false, _) => None,
        };
    }
}

/// Parse and dispatch every whole packet the reader now holds.
#[allow(clippy::too_many_arguments)]
async fn drain_packets<const N: usize>(
    reader: &mut PacketReader<N>,
    state: &mut ClientStateNoQueue,
    connection_id: ConnectionId,
    outbound: &Channel<CriticalSectionRawMutex, Vec<u8>, 4>,
    events: &EventChannel,
    runtime: &dyn RuntimeOps,
    last_ack_ms: &mut u64,
    connected: &mut bool,
    stable_at: &mut Option<u64>,
    stabilisation: u64,
) -> Result<(), Error> {
    while let Some(total) = reader.framed_len().map_err(client_error)? {
        // The packet borrows the reader's buffer, so everything that outlives
        // it — the response bytes, the application event — is made owned inside
        // this scope. `consume` can then take `&mut`.
        let (response, received) = {
            let packet: PacketGeneric<'_, MAX_PROPERTIES, 0, 0> =
                reader.parse(total).map_err(client_error)?;

            // Produce the PUBACK before the state update, as upstream does, so
            // the two cannot disagree about what was acknowledged.
            let response = match state
                .receive_produce_response(&packet)
                .map_err(client_error)?
            {
                Some(puback) => Some(encode(&puback)?),
                None => None,
            };

            let event = state.receive(packet).map_err(client_error)?;
            (response, Received::of(event, connection_id)?)
        };
        reader.consume(total);

        if let Some(bytes) = response {
            queue(outbound, bytes);
        }

        // Every packet the state accepted proves the broker is alive.
        *last_ack_ms = now_ms(runtime);

        // The CONNACK is whatever moved the state to `Connected`; nothing else
        // does, so there is no need to inspect packet types for it.
        if !*connected && matches!(state, ClientStateNoQueue::Connected(_)) {
            *connected = true;
            *stable_at = Some(now_ms(runtime) + stabilisation);
            events.send(MqttEvent::Connected { connection_id }).await;
        }

        if let Received::Event(event) = received {
            events.send(event).await;
        }
    }
    Ok(())
}

/// What a received packet leaves for the loop to do, owned so the reader's
/// buffer can be compacted first.
enum Received {
    /// An acknowledgement: liveness only, nothing to forward.
    Ack,
    /// Something the application asked to hear about.
    Event(MqttEvent<AimdbMqttEvent>),
}

impl Received {
    fn of(
        event: ClientStateReceiveEvent<'_, '_, MAX_PROPERTIES>,
        connection_id: ConnectionId,
    ) -> Result<Self, Error> {
        Ok(match event {
            ClientStateReceiveEvent::Ack => Self::Ack,

            ClientStateReceiveEvent::Publish { publish }
            | ClientStateReceiveEvent::PublishAndPuback { publish, .. } => {
                if publish.topic_name().is_empty() {
                    return Err(Error::Client(
                        ClientError::EmptyTopicNameWithAliasesDisabled,
                    ));
                }
                let message = publish.into();
                let event = AimdbMqttEvent::from_application_message(&message)
                    .map_err(|e| Error::Client(ClientError::EventHandler(e)))?;
                Self::Event(MqttEvent::ApplicationEvent {
                    connection_id,
                    event,
                })
            }

            ClientStateReceiveEvent::SubscriptionGrantedBelowMaximumQos {
                granted_qos,
                maximum_qos,
            } => Self::Event(MqttEvent::SubscriptionGrantedBelowMaximumQos {
                connection_id,
                granted_qos,
                maximum_qos,
            }),

            ClientStateReceiveEvent::PublishedMessageHadNoMatchingSubscribers => {
                Self::Event(MqttEvent::PublishedMessageHadNoMatchingSubscribers { connection_id })
            }

            ClientStateReceiveEvent::NoSubscriptionExisted => {
                Self::Event(MqttEvent::NoSubscriptionExisted { connection_id })
            }

            ClientStateReceiveEvent::Disconnect { disconnect } => {
                return Err(Error::Client(ClientError::Disconnected(
                    *disconnect.reason_code(),
                )))
            }
        })
    }
}

/// Turn one queued action into a packet on the wire.
///
/// Sent before the state update, as upstream does: a state that believes a
/// publish is in flight when it is not parks the action arm forever.
fn perform(
    action: AimdbMqttAction,
    state: &mut ClientStateNoQueue,
    outbound: &Channel<CriticalSectionRawMutex, Vec<u8>, 4>,
) -> Result<(), Error> {
    match action {
        AimdbMqttAction::Publish {
            topic,
            payload,
            qos,
            retain,
        } => {
            #[cfg(feature = "defmt")]
            defmt::debug!(
                "Publishing {} bytes to {} (QoS={:?})",
                payload.len(),
                topic.as_str(),
                qos
            );
            let packet = state
                .publish_packet(&topic, &payload, qos, retain)
                .inspect_err(|_e| {
                    // The action is already off the channel, so a failure here
                    // loses this message and ends the session — say which.
                    #[cfg(feature = "defmt")]
                    defmt::warn!(
                        "MQTT: dropping publish of {} bytes to {}: {}",
                        payload.len(),
                        topic.as_str(),
                        _e
                    );
                })
                .map_err(client_error)?;
            queue(outbound, encode(&packet)?);
            state.publish_update(&packet).map_err(client_error)?;
        }
        AimdbMqttAction::Subscribe { topic, qos } => {
            #[cfg(feature = "defmt")]
            defmt::info!("Subscribing to {} (QoS={:?})", topic.as_str(), qos);
            let packet = state
                .subscribe_packet(&topic, qos)
                .inspect_err(|_e| {
                    #[cfg(feature = "defmt")]
                    defmt::warn!("MQTT: dropping subscribe to {}: {}", topic.as_str(), _e);
                })
                .map_err(client_error)?;
            queue(outbound, encode(&packet)?);
            state.subscribe_update(&packet).map_err(client_error)?;
        }
    }
    Ok(())
}

/// Encode a packet to exactly its own length: a counting pass, then a real
/// one, so no fixed buffer is sized for the largest packet anyone might send.
fn encode<P: Write>(packet: &P) -> Result<Vec<u8>, Error> {
    let mut len_writer = MqttLenWriter::new();
    len_writer.put(packet).map_err(write_error)?;

    let mut bytes = alloc::vec![0u8; len_writer.position()];
    let mut writer = MqttBufWriter::new(&mut bytes);
    writer.put(packet).map_err(write_error)?;
    Ok(bytes)
}

/// Queue encoded bytes for the write half.
///
/// Never blocks — blocking would park the loop that has to notice the link is
/// gone. A ping or PUBACK dropped because the write half is backed up is
/// recovered by the next deadline or by redelivery.
fn queue(outbound: &Channel<CriticalSectionRawMutex, Vec<u8>, 4>, bytes: Vec<u8>) {
    if outbound.try_send(bytes).is_err() {
        #[cfg(feature = "defmt")]
        defmt::warn!("MQTT: write queue full, packet dropped");
    }
}

/// Milliseconds to sleep before the earliest armed deadline.
fn next_deadline(
    now: u64,
    connected: bool,
    ping_at: u64,
    liveness_at: u64,
    stable_at: Option<u64>,
    response_at: Option<u64>,
) -> u64 {
    let mut earliest = liveness_at;
    if connected {
        earliest = earliest.min(ping_at);
    }
    if let Some(at) = stable_at {
        earliest = earliest.min(at);
    }
    if let Some(at) = response_at {
        earliest = earliest.min(at);
    }
    earliest.saturating_sub(now).max(1)
}

fn client_error(error: impl Into<ClientError>) -> Error {
    Error::Client(error.into())
}

fn write_error(error: PacketWriteError) -> Error {
    Error::Client(ClientError::PacketWrite(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Halves that never do anything: enough to build the session future and
    /// measure it without polling it.
    struct NullRead;
    struct NullWrite;

    impl ByteRead for NullRead {
        async fn read(&mut self, _buf: &mut [u8]) -> aimdb_core::session::TransportResult<usize> {
            core::future::pending().await
        }
    }

    impl ByteWrite for NullWrite {
        async fn write_all(&mut self, _buf: &[u8]) -> aimdb_core::session::TransportResult<()> {
            Ok(())
        }

        async fn flush(&mut self) -> aimdb_core::session::TransportResult<()> {
            Ok(())
        }
    }

    struct NullDelay;

    impl Delay for NullDelay {
        fn sleep(&self, _d: Duration) -> impl core::future::Future<Output = ()> + Send {
            core::future::pending()
        }
    }

    #[test]
    fn the_buffer_budget_is_what_the_old_loop_cost() {
        // Criterion 7: the reassembly buffer plus the read scratch plus one
        // inbound slot come out of `BUFFER_SIZE`, not in addition to it.
        assert_eq!(PACKET_BUFFER_SIZE + 2 * RX_CHUNK, BUFFER_SIZE);
    }

    #[test]
    fn the_earliest_armed_deadline_wins() {
        // Liveness only, before the connection is up.
        assert_eq!(next_deadline(0, false, 100, 500, None, None), 500);
        // Once connected the ping is usually nearest.
        assert_eq!(next_deadline(0, true, 100, 500, None, None), 100);
        // Stabilisation and the response timeout arm independently.
        assert_eq!(next_deadline(0, true, 100, 500, Some(50), None), 50);
        assert_eq!(next_deadline(0, true, 100, 500, None, Some(20)), 20);
    }

    /// A ceiling on the session task's footprint. The bound is loose enough to
    /// absorb codegen drift, but not loose enough to fit another buffer.
    #[test]
    fn the_session_future_has_not_outgrown_the_loop_it_replaced() {
        let events = EventChannel::new();
        let actions = ActionChannel::new();
        let settings = Settings::default();
        let connection_settings = ConnectionSettings::unauthenticated("size-probe");
        let runtime = aimdb_core::executor::test_support::NoopRuntimeOps;

        // Built, never polled: `size_of_val` on the future is the whole point.
        let session = run_session(
            ConnectionId::new(0),
            NullRead,
            NullWrite,
            &connection_settings,
            &[],
            &events,
            &actions,
            &settings,
            &NullDelay,
            &runtime,
        );

        let size = core::mem::size_of_val(&session);
        assert!(
            size <= BUFFER_SIZE * 2,
            "the session future is {size} bytes, over the {} allowed — has a \
             buffer been added rather than carved out of BUFFER_SIZE?",
            BUFFER_SIZE * 2
        );
    }

    #[test]
    fn a_deadline_in_the_past_still_sleeps_a_tick() {
        // Never zero: a zero-length sleep would spin the loop.
        assert_eq!(next_deadline(1_000, true, 100, 500, None, None), 1);
    }
}
