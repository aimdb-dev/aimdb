//! The event-driven broker session: three futures in one `select3`.
//!
//! The stream is split and the only thing ever cancelled is a channel receive,
//! so the non-cancel-safe `write_all` never sits in a `select` arm and no
//! partially-read packet is ever discarded — which is what lets the TLS path
//! share this loop.
//!
//! [`read_into`] and [`WriteRing::write_out`] know no MQTT; [`client_loop`]
//! owns all client state and wakes only on data, an action or a deadline. Raw
//! chunks cross the inbound channel rather than whole packets, so framing
//! needs no second packet-sized buffer. Outbound packets are encoded straight
//! into the connector's [`WriteRing`].

use core::convert::Infallible;
use core::future::poll_fn;
use core::task::Poll;
use core::time::Duration;

use aimdb_core::session::{ByteRead, ByteWrite, Delay};
use aimdb_core::{InboundDispatch, OutboundRoutes, RouteId, RuntimeOps};
use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use mountain_mqtt::client::{ClientError, ConnectionSettings};
use mountain_mqtt::client_state::{ClientState, ClientStateNoQueue, ClientStateReceiveEvent};
use mountain_mqtt::data::packet_identifier::{PacketIdentifier, PublishPacketIdentifier};
use mountain_mqtt::data::property::{ConnackProperty, ConnectProperty, Property};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::error::{PacketReadError, PacketWriteError};
use mountain_mqtt::packets::connect::Connect;
use mountain_mqtt::packets::packet_generic::PacketGeneric;
use mountain_mqtt::packets::publish::Publish;
use mountain_mqtt::packets::subscribe::{Subscribe, SubscriptionRequest};

use crate::embedded::manager::{now_ms, Error, Settings};
use crate::embedded::packet_reader::PacketReader;
use crate::embedded::write_ring::{encoded_len, WriteRing, CONTROL_RESERVE};
use crate::embedded::{BUFFER_SIZE, MAX_PROPERTIES};
use crate::publish_opts::PublishOpts;

/// Bytes lifted off the socket at a time, and the size of one `inbound` slot.
const RX_CHUNK: usize = 256;

/// The largest MQTT packet the session can receive.
///
/// Reassembly and the inbound slots come out of one `BUFFER_SIZE`; outbound
/// packets go into the connector's write ring instead.
const PACKET_BUFFER_SIZE: usize = BUFFER_SIZE - 2 * RX_CHUNK;

/// Ring space waited for before a QoS 1 publish is parsed: enough for its
/// PUBACK (6 bytes; mountain-mqtt adds no properties).
const PUBACK_ROOM: usize = 16;

/// The largest packet the reader takes whatever arrives before it: the
/// buffer minus one feed chunk (see [`PacketReader`]). Advertised as the
/// CONNECT's Maximum Packet Size, so a broker never sends a larger packet —
/// one that would end the session, and a retained one would end every
/// session it is replayed into.
const MAX_INBOUND_PACKET: usize = PACKET_BUFFER_SIZE - RX_CHUNK;

/// One chunk of freshly read bytes, in flight from the read half to the loop.
type Chunk = heapless::Vec<u8, RX_CHUNK>;

/// Drive one MQTT session over a split stream until an error ends it.
///
/// Connects, subscribes `subscribe_topics`, then publishes what `outbound`
/// stages and dispatches every inbound publish into its records through
/// `dispatch`. Returns only on failure — the caller reconnects.
///
/// `ring` is the connector's, reused across sessions; whatever an old session
/// left in it is discarded first, so nothing reaches the new socket ahead of
/// its CONNECT.
///
/// **At most once**: a message is taken from its record buffer before it is
/// written, so the one in flight when a session ends is lost. Everything still
/// in the record buffers survives (what survives depends on each buffer's
/// type).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_session<R, W, D>(
    rx: R,
    tx: W,
    connection_settings: &ConnectionSettings<'static>,
    subscribe_topics: &[(&str, QualityOfService)],
    dispatch: &InboundDispatch,
    outbound: &mut OutboundRoutes,
    opts: &[PublishOpts],
    ring: &WriteRing,
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
    ring.drain();

    let session = client_loop(
        &inbound,
        ring,
        connection_settings,
        subscribe_topics,
        dispatch,
        outbound,
        opts,
        settings,
        delay,
        runtime,
    );

    match select3(read_into(rx, &inbound), ring.write_out(tx), session).await {
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

fn receive_failed() -> Error {
    Error::Client(ClientError::PacketRead(PacketReadError::ConnectionReceive))
}

/// Everything the session knows, in one future: state, framing, deadlines.
#[allow(clippy::too_many_arguments)]
async fn client_loop<D: Delay>(
    inbound: &Channel<CriticalSectionRawMutex, Chunk, 1>,
    ring: &WriteRing,
    connection_settings: &ConnectionSettings<'static>,
    subscribe_topics: &[(&str, QualityOfService)],
    dispatch: &InboundDispatch,
    outbound: &mut OutboundRoutes,
    opts: &[PublishOpts],
    settings: &Settings,
    delay: &D,
    runtime: &dyn RuntimeOps,
) -> Result<Infallible, Error> {
    let ping_interval = settings.ping_interval.as_millis() as u64;
    let max_silence = settings.connection_event_max_interval.as_millis() as u64;
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
    let mut connected = false;
    let mut next_topic = 0usize;
    // The broker's Maximum Packet Size, from its CONNACK. Sending a larger
    // packet is a protocol error that ends the session.
    let mut broker_max: Option<usize> = None;
    // Latched on `Ready(None)`: every route is closed, or there were none.
    // Passing it through would resolve the arm on every iteration and the
    // session would never yield.
    let mut outbound_done = false;

    // CONNECT goes out first; its CONNACK is what flips `connected`.
    {
        let connect = connect_packet(settings, connection_settings);
        state.connect(&connect).map_err(client_error)?;
        ring.put(&connect, 0).await?;
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

        if connected && now.saturating_sub(last_ping_ms) >= ping_interval {
            last_ping_ms = now;
            let ping = state.send_ping().map_err(client_error)?;
            // The one packet worth dropping rather than waiting for. A ping
            // carries no state — `send_ping` bumps a counter but arms no
            // response deadline — so a dropped one costs nothing and the next
            // deadline tries again; if the link really is gone, the liveness
            // window closes the session. Parking on a ping would leave the
            // loop that has to notice a dead link stuck.
            if !ring.try_put(&ping)? {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT: write ring full, ping dropped");
            }
        }

        // Subscriptions go out one at a time: `ClientStateNoQueue` tracks a
        // single outstanding request, so the next one waits for this SUBACK.
        if connected && !state.waiting_for_responses() && next_topic < subscribe_topics.len() {
            let (topic, qos) = subscribe_topics[next_topic];
            let packet = state.subscribe_packet(topic, qos).map_err(client_error)?;
            ring.put(&packet, 0).await?;
            state.subscribe_update(&packet).map_err(client_error)?;
            next_topic += 1;
            // `continue` skips the bottom-of-loop bookkeeping, so arm the
            // response deadline here: a broker that never SUBACKs should be
            // caught by `response_timeout`, not only by the liveness window.
            waiting_since = Some(now);
            continue;
        }

        // --- park until something happens ----------------------------------

        // The publish arm is armed only when a publish can actually be sent:
        // connected, nothing awaiting acknowledgement (the client state holds
        // one in-flight slot), every subscription placed, and room in the ring
        // for the largest PUBLISH plus its reserve. The ping and liveness
        // deadlines keep running while it is parked. Room is checked on every
        // poll, not once here: PUBACKs and pings can take ring space while the
        // arm is parked. A value leaves its record buffer only when the arm
        // resolves, so a losing arm takes nothing.
        let publish_ready = connected
            && !outbound_done
            && !state.waiting_for_responses()
            && next_topic >= subscribe_topics.len();
        let publish_room = ring.max_publish() + CONTROL_RESERVE;
        let publish_arm = poll_fn(|cx| {
            if !publish_ready || !ring.poll_room(publish_room, cx.waker()) {
                return Poll::Pending;
            }
            match outbound.poll_stage(cx) {
                Poll::Ready(Some(id)) => Poll::Ready(id),
                Poll::Ready(None) => {
                    outbound_done = true;
                    Poll::Pending
                }
                Poll::Pending => Poll::Pending,
            }
        });

        let sleep_for = Duration::from_millis(next_deadline(
            now,
            connected,
            last_ping_ms + ping_interval,
            last_ack_ms + max_silence,
            waiting_since.map(|since| since + response_timeout),
        ));

        match select3(inbound.receive(), publish_arm, delay.sleep(sleep_for)).await {
            Either3::First(chunk) => {
                reader.feed(&chunk).map_err(client_error)?;
                drain_packets(
                    &mut reader,
                    &mut state,
                    ring,
                    dispatch,
                    runtime,
                    &mut last_ack_ms,
                    &mut connected,
                    &mut broker_max,
                )
                .await?;
            }
            Either3::Second(id) => {
                publish_staged(outbound, id, opts, &mut state, ring, broker_max).await?;
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

/// Parse every whole packet the reader now holds, dispatching publishes into
/// their records.
#[allow(clippy::too_many_arguments)]
async fn drain_packets<const N: usize>(
    reader: &mut PacketReader<N>,
    state: &mut ClientStateNoQueue,
    ring: &WriteRing,
    dispatch: &InboundDispatch,
    runtime: &dyn RuntimeOps,
    last_ack_ms: &mut u64,
    connected: &mut bool,
    broker_max: &mut Option<usize>,
) -> Result<(), Error> {
    while let Some(total) = reader.framed_len().map_err(client_error)? {
        // A burst of QoS 1 publishes needs a PUBACK each, so room for one is
        // waited for here, before parsing: the parsed packet is too large to
        // hold across an await in the session future.
        if reader.head_needs_ack() {
            ring.wait_room(PUBACK_ROOM).await;
        }

        // The packet borrows the reader's buffer, so it is handled entirely
        // inside this scope; `consume` can then take `&mut`.
        {
            let packet: PacketGeneric<'_, MAX_PROPERTIES, 0, 0> =
                reader.parse(total).map_err(client_error)?;

            if let PacketGeneric::Connack(connack) = &packet {
                *broker_max = connack.properties().iter().find_map(|p| match p {
                    ConnackProperty::MaximumPacketSize(max) => Some(max.value() as usize),
                    _ => None,
                });
            }

            // Produce the PUBACK before the state update, as upstream does, so
            // the two cannot disagree about what was acknowledged. It borrows
            // the client state, so it goes into the ring before
            // `state.receive` takes `&mut`. The room was waited for above and
            // nothing else writes the ring in between.
            if let Some(puback) = state
                .receive_produce_response(&packet)
                .map_err(client_error)?
            {
                if !ring.try_put(&puback)? {
                    return Err(client_error(PacketWriteError::Overflow));
                }
            }

            let event = state.receive(packet).map_err(client_error)?;
            deliver(event, dispatch)?;
        }
        reader.consume(total);

        // Every packet the state accepted proves the broker is alive.
        *last_ack_ms = now_ms(runtime);

        // The CONNACK is whatever moved the state to `Connected`; nothing else
        // does, so there is no need to inspect packet types for it.
        if !*connected && matches!(state, ClientStateNoQueue::Connected(_)) {
            *connected = true;
        }
    }
    Ok(())
}

/// Hand a received publish to its records; everything else only proves
/// liveness.
///
/// The PUBACK for a QoS 1 publish is already queued, so a record buffer that
/// is full drops a message the broker considers delivered.
fn deliver(
    event: ClientStateReceiveEvent<'_, '_, MAX_PROPERTIES>,
    dispatch: &InboundDispatch,
) -> Result<(), Error> {
    match event {
        ClientStateReceiveEvent::Ack => {}

        ClientStateReceiveEvent::Publish { publish }
        | ClientStateReceiveEvent::PublishAndPuback { publish, .. } => {
            if publish.topic_name().is_empty() {
                return Err(Error::Client(
                    ClientError::EmptyTopicNameWithAliasesDisabled,
                ));
            }
            #[cfg(feature = "defmt")]
            defmt::debug!(
                "Received message on topic '{}', {} bytes",
                publish.topic_name(),
                publish.payload().len()
            );
            dispatch.dispatch(publish.topic_name(), publish.payload());
        }

        // Liveness, and nothing else. The broker is telling us a subscription
        // was granted below the QoS asked for, that a publish matched no
        // subscriber, or that an unsubscribe named a subscription it did not
        // hold. A record has no connection-state callback to deliver it to.
        ClientStateReceiveEvent::SubscriptionGrantedBelowMaximumQos { .. }
        | ClientStateReceiveEvent::PublishedMessageHadNoMatchingSubscribers
        | ClientStateReceiveEvent::NoSubscriptionExisted => {}

        ClientStateReceiveEvent::Disconnect { disconnect } => {
            return Err(Error::Client(ClientError::Disconnected(
                *disconnect.reason_code(),
            )))
        }
    }
    Ok(())
}

/// Publish the message `outbound` staged for route `id`.
///
/// Sent before the state update, as upstream does: a state that believes a
/// publish is in flight when it is not parks the publish arm forever. A frame
/// the ring can never grant, or larger than the broker accepts, is skipped
/// before the state commits to it, logged, and counted as rejected in the
/// route's `RouteStats`.
async fn publish_staged(
    outbound: &mut OutboundRoutes,
    id: RouteId,
    opts: &[PublishOpts],
    state: &mut ClientStateNoQueue,
    ring: &WriteRing,
    broker_max: Option<usize>,
) -> Result<(), Error> {
    let opt = opts.get(id).copied().unwrap_or(PublishOpts {
        qos: 1,
        retain: false,
    });
    // QoS 2 is not supported by this client; build() warned once per route.
    let qos = if opt.qos == 0 {
        QualityOfService::Qos0
    } else {
        QualityOfService::Qos1
    };
    let rejected = {
        let Some(msg) = outbound.take_staged() else {
            return Ok(());
        };
        let packet = state
            .publish_packet(msg.topic, msg.payload.as_slice(), qos, opt.retain)
            .map_err(client_error)?;
        let len = encoded_len(&packet)?;
        let over_broker = broker_max.is_some_and(|max| len > max);
        if !ring.fits(len, CONTROL_RESERVE) || over_broker {
            aimdb_core::log_warn!(
                "MQTT: skipping publish to '{}': {} bytes exceed the {} limit",
                msg.topic,
                len,
                if over_broker {
                    "broker's"
                } else {
                    "write ring's"
                }
            );
            #[cfg(feature = "defmt")]
            defmt::warn!(
                "MQTT: skipping publish to {}: {} bytes exceed the {} limit",
                msg.topic,
                len,
                if over_broker {
                    "broker's"
                } else {
                    "write ring's"
                }
            );
            true
        } else {
            ring.put_sized(&packet, len, CONTROL_RESERVE).await?;
            state.publish_update(&packet).map_err(client_error)?;
            false
        }
    };
    if rejected {
        outbound.reject(id);
    }
    Ok(())
}

/// The CONNECT this session sends, also sized at build.
pub(crate) fn connect_packet<'a>(
    settings: &Settings,
    connection_settings: &'a ConnectionSettings<'static>,
) -> Connect<'a, 2, 0> {
    let mut properties = heapless::Vec::new();
    // Topic aliases are declined: honouring them would mean storing the
    // server's topic names for the life of the connection.
    let _ = properties.push(ConnectProperty::TopicAliasMaximum(0.into()));
    let _ = properties.push(ConnectProperty::MaximumPacketSize(
        (MAX_INBOUND_PACKET as u32).into(),
    ));
    // Ours, not `connection_settings.keep_alive()`: that field has no setter,
    // so it is always mountain-mqtt's own 60 s constant. The cadence is
    // derived from the value we actually send.
    Connect::new(
        settings.keep_alive_secs,
        *connection_settings.username(),
        *connection_settings.password(),
        connection_settings.client_id(),
        true,
        None,
        properties,
    )
}

/// Bytes the SUBSCRIBE for `topic` encodes to.
pub(crate) fn subscribe_len(topic: &str) -> Result<usize, Error> {
    let packet: Subscribe<'_, 0, 0> = Subscribe::new(
        PacketIdentifier(1),
        SubscriptionRequest::new(topic, QualityOfService::Qos1),
        heapless::Vec::new(),
        heapless::Vec::new(),
    );
    encoded_len(&packet)
}

/// Bytes a QoS 1 PUBLISH with a `topic_len`-byte topic and a
/// `payload_len`-byte payload encodes to: the largest frame a route with
/// those capacities produces.
pub(crate) fn publish_frame_len(topic_len: usize, payload_len: usize) -> Result<usize, Error> {
    let topic = "x".repeat(topic_len);
    let payload = alloc::vec![0u8; payload_len];
    let packet: Publish<'_, 0> = Publish::new(
        false,
        false,
        &topic,
        PublishPacketIdentifier::Qos1(PacketIdentifier(1)),
        &payload,
        heapless::Vec::new(),
    );
    encoded_len(&packet)
}

/// Milliseconds to sleep before the earliest armed deadline.
fn next_deadline(
    now: u64,
    connected: bool,
    ping_at: u64,
    liveness_at: u64,
    response_at: Option<u64>,
) -> u64 {
    let mut earliest = liveness_at;
    if connected {
        earliest = earliest.min(ping_at);
    }
    if let Some(at) = response_at {
        earliest = earliest.min(at);
    }
    earliest.saturating_sub(now).max(1)
}

fn client_error(error: impl Into<ClientError>) -> Error {
    Error::Client(error.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

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
        // The reassembly buffer plus the read scratch plus one
        // inbound slot come out of `BUFFER_SIZE`, not in addition to it.
        assert_eq!(PACKET_BUFFER_SIZE + 2 * RX_CHUNK, BUFFER_SIZE);
    }

    #[test]
    fn the_earliest_armed_deadline_wins() {
        // Liveness only, before the connection is up.
        assert_eq!(next_deadline(0, false, 100, 500, None), 500);
        // Once connected the ping is usually nearest.
        assert_eq!(next_deadline(0, true, 100, 500, None), 100);
        // The response timeout arms independently.
        assert_eq!(next_deadline(0, true, 100, 500, Some(20)), 20);
    }

    /// A ceiling on the session task's footprint. The bound is loose enough to
    /// absorb codegen drift, but not loose enough to fit another buffer.
    #[test]
    fn the_session_future_has_not_outgrown_the_loop_it_replaced() {
        let settings = Settings::default();
        let connection_settings = ConnectionSettings::unauthenticated("size-probe");
        let runtime = aimdb_core::executor::test_support::NoopRuntimeOps;
        let ring = WriteRing::new(64);
        let (db, _runner) = futures::executor::block_on(
            aimdb_core::AimDbBuilder::new()
                .runtime(alloc::sync::Arc::new(runtime))
                .build(),
        )
        .expect("empty database");
        let dispatch = InboundDispatch::new(&db, "mqtt", &crate::MqttGrammar).expect("no links");
        let mut outbound = OutboundRoutes::new(&db, "mqtt").expect("no links");

        // Built, never polled: `size_of_val` on the future is the whole point.
        let session = run_session(
            NullRead,
            NullWrite,
            &connection_settings,
            &[],
            &dispatch,
            &mut outbound,
            &[],
            &ring,
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

    /// A v5 PUBLISH on topic `t` carrying `n` user properties.
    fn publish_with_properties(n: usize) -> Vec<u8> {
        fn varint(mut n: usize, out: &mut Vec<u8>) {
            loop {
                let mut byte = (n % 128) as u8;
                n /= 128;
                if n > 0 {
                    byte |= 128;
                }
                out.push(byte);
                if n == 0 {
                    return;
                }
            }
        }

        // Each one is `0x26` then two length-prefixed strings.
        let mut properties = Vec::new();
        for _ in 0..n {
            properties.extend_from_slice(&[0x26, 0x00, 0x01, b'k', 0x00, 0x01, b'v']);
        }

        let mut rest = Vec::new();
        rest.extend_from_slice(&1u16.to_be_bytes());
        rest.push(b't');
        varint(properties.len(), &mut rest);
        rest.extend_from_slice(&properties);
        rest.extend_from_slice(b"x");

        let mut packet = alloc::vec![0x30u8];
        varint(rest.len(), &mut packet);
        packet.extend_from_slice(&rest);
        packet
    }

    /// The property cap is where `MAX_PROPERTIES` says, and one past it is an
    /// error rather than a silent truncation.
    ///
    /// The cap applies to every received packet, and on an inbound publish it
    /// is the *publishing peer* who decides how many properties to attach. One
    /// too many ends the session, so a retained publish over the cap is
    /// replayed on every resubscribe and reconnect-loops the connector — the
    /// same shape as an over-large packet.
    #[test]
    fn one_property_past_the_cap_is_refused_rather_than_truncated() {
        let mut reader = PacketReader::<4096>::new();

        let at_cap = publish_with_properties(MAX_PROPERTIES);
        reader.feed(&at_cap).expect("feed");
        let total = reader.framed_len().expect("framing").expect("complete");
        assert!(
            reader.parse::<MAX_PROPERTIES, 0, 0>(total).is_ok(),
            "a publish at the cap must parse"
        );
        reader.consume(total);

        let over_cap = publish_with_properties(MAX_PROPERTIES + 1);
        reader.feed(&over_cap).expect("feed");
        let total = reader.framed_len().expect("framing").expect("complete");
        assert_eq!(
            reader
                .parse::<MAX_PROPERTIES, 0, 0>(total)
                .err()
                .map(|e| alloc::format!("{e:?}")),
            Some(alloc::string::String::from("TooManyProperties")),
            "one property past the cap must be refused, not quietly dropped"
        );
    }

    /// A QoS 0 PUBLISH to `t` that is exactly `total` bytes on the wire.
    fn publish_of(total: usize) -> Vec<u8> {
        let varint_len = if total - 2 < 128 { 1 } else { 2 };
        let remaining = total - 1 - varint_len;
        let mut bytes = alloc::vec![0x30u8];
        if varint_len == 1 {
            bytes.push(remaining as u8);
        } else {
            bytes.push((remaining % 128) as u8 | 0x80);
            bytes.push((remaining / 128) as u8);
        }
        bytes.extend_from_slice(&[0x00, 0x01, b't', 0x00]);
        bytes.resize(total, b'x');
        bytes
    }

    /// Feeds a `first`-byte packet, a `second`-byte one and a trailing one in
    /// `RX_CHUNK` reads, consuming packets as they complete, as the session
    /// does. The trailing packet makes the read that completes `second` a full
    /// one that also carries the head of the next packet: the worst case.
    fn receive_after(first: usize, second: usize) -> Result<(), PacketReadError> {
        let mut stream = publish_of(first);
        stream.extend_from_slice(&publish_of(second));
        stream.extend_from_slice(&publish_of(RX_CHUNK));
        let mut reader = PacketReader::<PACKET_BUFFER_SIZE>::new();
        let mut received = 0;
        for chunk in stream.chunks(RX_CHUNK) {
            reader.feed(chunk)?;
            while let Some(total) = reader.framed_len()? {
                reader.consume(total);
                received += 1;
            }
        }
        assert!(received >= 2);
        Ok(())
    }

    /// The advertised Maximum Packet Size is one the reader takes wherever the
    /// packet starts inside a read. The reader's stated limit (buffer minus
    /// one read) is conservative by one byte; two bytes more fail at some
    /// offset.
    #[test]
    fn the_advertised_maximum_packet_size_is_always_received() {
        assert_eq!(MAX_INBOUND_PACKET, 3328);
        let offsets = 8..8 + RX_CHUNK;
        for first in offsets.clone() {
            assert_eq!(
                receive_after(first, MAX_INBOUND_PACKET),
                Ok(()),
                "after {first} bytes"
            );
        }
        assert!(offsets
            .map(|first| receive_after(first, MAX_INBOUND_PACKET + 2))
            .any(|r| r == Err(PacketReadError::PacketTooLargeForBuffer)));
    }

    #[test]
    fn puback_room_covers_the_puback_the_client_state_produces() {
        use mountain_mqtt::data::packet_identifier::PacketIdentifier;
        use mountain_mqtt::data::reason_code::PublishReasonCode;
        use mountain_mqtt::packets::puback::Puback;

        let puback: Puback<'_, MAX_PROPERTIES> = Puback::new(
            PacketIdentifier(u16::MAX),
            PublishReasonCode::Success,
            heapless::Vec::new(),
        );
        assert!(encoded_len(&puback).unwrap() <= PUBACK_ROOM);
    }

    #[test]
    fn a_deadline_in_the_past_still_sleeps_a_tick() {
        // Never zero: a zero-length sleep would spin the loop.
        assert_eq!(next_deadline(1_000, true, 100, 500, None), 1);
    }
}

#[cfg(test)]
mod proofs {
    //! Where the embedded backend's size limits sit: the largest PUBLISH it
    //! sends, what it does with a larger one (skipped and counted as rejected
    //! in the route's `RouteStats`), and the largest packet it receives.
    use super::*;
    use crate::embedded::write_ring::DEFAULT_WRITE_BUFFER;
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use mountain_mqtt::data::reason_code::ConnectReasonCode;
    use mountain_mqtt::packets::connack::Connack;

    fn connected() -> ClientStateNoQueue {
        let mut state = ClientStateNoQueue::new();
        let connect: Connect<'_, 1, 0> =
            Connect::new(60, None, None, "proof", true, None, heapless::Vec::new());
        state.connect(&connect).unwrap();
        let connack: Connack<'_, MAX_PROPERTIES> =
            Connack::new(false, ConnectReasonCode::Success, heapless::Vec::new());
        state
            .receive(PacketGeneric::<'_, MAX_PROPERTIES, 0, 0>::Connack(connack))
            .unwrap();
        state
    }

    /// Payload length whose QoS 1 PUBLISH to `t` encodes to exactly `frame`.
    fn payload_for(frame: usize) -> usize {
        let mut probe = connected();
        (0..frame)
            .rev()
            .find(|&n| {
                let payload = alloc::vec![0u8; n];
                let packet = probe
                    .publish_packet("t", &payload, QualityOfService::Qos1, false)
                    .unwrap();
                encoded_len(&packet).unwrap() == frame
            })
            .unwrap()
    }

    /// Lets `link_to("mqtt://…")` register; drives nothing.
    struct NoTransport;

    impl aimdb_core::connector::ConnectorBuilder for NoTransport {
        #[allow(clippy::type_complexity)]
        fn build<'a>(
            &'a self,
            _db: &'a aimdb_core::AimDb,
        ) -> core::pin::Pin<
            Box<
                dyn core::future::Future<
                        Output = aimdb_core::DbResult<
                            Vec<core::pin::Pin<Box<dyn core::future::Future<Output = ()> + Send>>>,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn scheme(&self) -> &str {
            "mqtt"
        }
    }

    /// One route to `t` whose owned serializer emits as many bytes as the
    /// value says, with a message of `payload_len` bytes staged.
    async fn staged(payload_len: usize) -> (aimdb_core::AimDb, OutboundRoutes, RouteId) {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

        let mut builder = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(NoTransport);
        builder.configure::<usize>("blob", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_to("mqtt://t")
                .with_serializer(|_ctx, n: &usize| Ok(alloc::vec![b'x'; *n]))
                .finish();
        });
        let (db, _runner) = builder.build().await.expect("build");
        let mut outbound = OutboundRoutes::new(&db, "mqtt").expect("routes");
        db.produce("blob", payload_len).expect("produce");
        let id = poll_fn(|cx| outbound.poll_stage(cx))
            .await
            .expect("route open");
        (db, outbound, id)
    }

    const QOS1: [PublishOpts; 1] = [PublishOpts {
        qos: 1,
        retain: false,
    }];

    #[tokio::test]
    async fn a_1984_byte_publish_goes_out() {
        let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
        assert_eq!(ring.max_publish(), 1984);
        let mut state = connected();
        let (_db, mut outbound, id) = staged(payload_for(1984)).await;
        publish_staged(&mut outbound, id, &QOS1, &mut state, &ring, None)
            .await
            .unwrap();
        assert!(!ring.has_room(DEFAULT_WRITE_BUFFER), "bytes were queued");
        assert!(state.waiting_for_responses(), "QoS 1 publish in flight");
        assert_eq!(outbound.stats(id).unwrap().rejected, 0);
    }

    #[tokio::test]
    async fn a_1985_byte_publish_is_skipped_and_counted() {
        let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
        let mut state = connected();
        let (_db, mut outbound, id) = staged(payload_for(1985)).await;
        publish_staged(&mut outbound, id, &QOS1, &mut state, &ring, None)
            .await
            .unwrap();
        assert!(ring.has_room(DEFAULT_WRITE_BUFFER), "nothing was queued");
        assert!(!state.waiting_for_responses(), "nothing in flight");
        let stats = outbound.stats(id).unwrap();
        assert_eq!((stats.sent, stats.rejected), (1, 1));
    }

    #[tokio::test]
    async fn a_publish_over_the_brokers_maximum_packet_size_is_skipped_and_counted() {
        let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
        let mut state = connected();
        let (_db, mut outbound, id) = staged(payload_for(200)).await;
        publish_staged(&mut outbound, id, &QOS1, &mut state, &ring, Some(199))
            .await
            .unwrap();
        assert!(ring.has_room(DEFAULT_WRITE_BUFFER), "nothing was queued");
        assert_eq!(outbound.stats(id).unwrap().rejected, 1);

        // At the limit it goes out.
        let (_db, mut outbound, id) = staged(payload_for(200)).await;
        publish_staged(&mut outbound, id, &QOS1, &mut state, &ring, Some(200))
            .await
            .unwrap();
        assert_eq!(outbound.stats(id).unwrap().rejected, 0);
        assert!(state.waiting_for_responses());
    }

    #[test]
    fn publish_frame_len_matches_what_the_client_state_encodes() {
        let mut state = connected();
        for (topic_len, payload_len) in [(1, 0), (1, 100), (30, 1900), (200, 3000)] {
            let topic = "t".repeat(topic_len);
            let payload = alloc::vec![0u8; payload_len];
            let packet = state
                .publish_packet(&topic, &payload, QualityOfService::Qos1, true)
                .unwrap();
            assert_eq!(
                publish_frame_len(topic_len, payload_len).unwrap(),
                encoded_len(&packet).unwrap(),
                "{topic_len}-byte topic, {payload_len}-byte payload"
            );
        }
    }

    /// A QoS 0 PUBLISH to `t` that is `total` bytes on the wire.
    fn inbound_publish(total: usize) -> Vec<u8> {
        let remaining = total - 3; // header byte + 2-byte varint
        let mut bytes = alloc::vec![
            0x30,
            (remaining % 128) as u8 | 0x80,
            (remaining / 128) as u8,
            0x00,
            0x01,
            b't',
            0x00,
        ];
        bytes.resize(total, b'x');
        bytes
    }

    fn receive(total: usize) -> Result<usize, PacketReadError> {
        let mut reader = PacketReader::<PACKET_BUFFER_SIZE>::new();
        for chunk in inbound_publish(total).chunks(RX_CHUNK) {
            reader.feed(chunk)?;
            if let Some(n) = reader.framed_len()? {
                return Ok(n);
            }
        }
        unreachable!("the whole packet was fed")
    }

    #[test]
    fn proof_the_receive_side_takes_3584_and_refuses_3585() {
        assert_eq!(PACKET_BUFFER_SIZE, 3584);
        assert_eq!(receive(3584), Ok(3584));
        assert_eq!(receive(3585), Err(PacketReadError::PacketTooLargeForBuffer));
    }
}
