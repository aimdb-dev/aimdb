//! What the event-driven session promises that the polled one could not
//! (design 053 §10, criteria 1, 2, 4 and 11).
//!
//! These are behavioural, not smoke: every one of them passes trivially on a
//! loop that polls at 100 Hz and blocks inline for acknowledgements, or fails
//! outright on it. The broker here is scripted rather than the shared
//! `common::fake_broker`, because each test needs to control *when* it answers
//! — mid-packet, late, or not at all.
#![cfg(feature = "_test-tokio-broker")]

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::session::{Delay, StreamDialer, TransportResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Each test binary defines these exactly once.
#[defmt::global_logger]
struct HostTestLogger;
unsafe impl defmt::Logger for HostTestLogger {
    fn acquire() {}
    unsafe fn flush() {}
    unsafe fn release() {}
    unsafe fn write(_bytes: &[u8]) {}
}
#[defmt::panic_handler]
fn defmt_panic() -> ! {
    core::panic!("defmt panic in host test")
}
defmt::timestamp!("{=u64:us}", 0);

/// Real wall-clock time for `embassy-time`, which the test's dependency graph
/// links even though the session loop itself runs on core's `Delay`.
struct HostClock;
impl embassy_time_driver::Driver for HostClock {
    fn now(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        (start.elapsed().as_micros() * u128::from(embassy_time_driver::TICK_HZ) / 1_000_000) as u64
    }
    fn schedule_wake(&self, _at: u64, waker: &core::task::Waker) {
        waker.wake_by_ref();
    }
}
embassy_time_driver::time_driver_impl!(static HOST_CLOCK: HostClock = HostClock);

// ---------------------------------------------------------------------------
// A dialer that counts what the session sleeps on.
// ---------------------------------------------------------------------------

/// `TokioNet::tcp()` with a tally of every `Delay::sleep` the connector asks
/// for. The connector takes its clock from the dialer, so this is the seam
/// where "how often does the session wake?" is observable at all.
#[derive(Clone)]
struct CountingDialer {
    inner: aimdb_tokio_adapter::net::TokioTcpDialer,
    sleeps: Arc<AtomicUsize>,
}

impl CountingDialer {
    fn new() -> Self {
        Self {
            inner: aimdb_tokio_adapter::net::TokioNet::tcp(),
            sleeps: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl StreamDialer for CountingDialer {
    type Stream = <aimdb_tokio_adapter::net::TokioTcpDialer as StreamDialer>::Stream;

    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a {
        self.inner.connect(host, port)
    }
}

impl Delay for CountingDialer {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
        self.sleeps.fetch_add(1, Ordering::Relaxed);
        Delay::sleep(&self.inner, d)
    }
}

// ---------------------------------------------------------------------------
// A scripted broker: the same wire format as `common`, but the test decides
// when each answer goes out.
// ---------------------------------------------------------------------------

/// What the scripted broker saw, and when.
#[derive(Default)]
struct Log {
    pings: usize,
    /// Client publishes, as (topic, payload).
    publishes: Vec<(String, Vec<u8>)>,
    /// Pings that arrived while the broker was deliberately stalling.
    pings_during_stall: usize,
    /// Client publishes that arrived while the broker was stalling.
    publishes_during_stall: usize,
}

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

/// An MQTT 5 PUBLISH at QoS 0.
fn publish_packet(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut rest = Vec::new();
    rest.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    rest.extend_from_slice(topic.as_bytes());
    rest.push(0x00); // no properties
    rest.extend_from_slice(payload);

    let mut packet = vec![0x30];
    varint(rest.len(), &mut packet);
    packet.extend_from_slice(&rest);
    packet
}

/// Read one packet: header byte, varint remaining length, body.
async fn read_one<S: tokio::io::AsyncRead + Unpin>(socket: &mut S) -> Option<(u8, Vec<u8>)> {
    let mut byte = [0u8; 1];
    socket.read_exact(&mut byte).await.ok()?;
    let first = byte[0];

    let mut remaining = 0usize;
    let mut shift = 0;
    loop {
        socket.read_exact(&mut byte).await.ok()?;
        remaining |= ((byte[0] & 0x7F) as usize) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    let mut body = vec![0u8; remaining];
    socket.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

/// Pull the topic and payload out of a client PUBLISH, and its packet id when
/// it carries one (QoS > 0).
fn parse_publish(first: u8, body: &[u8]) -> Option<(String, Vec<u8>, Option<[u8; 2]>)> {
    let topic_len = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize;
    let topic = String::from_utf8_lossy(body.get(2..2 + topic_len)?).into_owned();
    let mut i = 2 + topic_len;

    let packet_id = if (first >> 1) & 0x03 > 0 {
        let id = [*body.get(i)?, *body.get(i + 1)?];
        i += 2;
        Some(id)
    } else {
        None
    };

    // MQTT 5 property length (always short here).
    let property_len = *body.get(i)? as usize;
    i += 1 + property_len;

    Some((topic, body.get(i..)?.to_vec(), packet_id))
}

/// How the scripted broker should misbehave after it has SUBACKed.
#[derive(Clone, Copy)]
enum Script {
    /// Answer nothing but pings: an idle, healthy session.
    Idle,
    /// Push a PUBLISH split in two with `gap` between the halves.
    SplitPublish { gap: Duration },
    /// Hold every PUBACK back by `delay`.
    SlowPuback { delay: Duration },
    /// Push inbound PUBLISHes as fast as they will go, for `duration`.
    Flood { duration: Duration },
}

/// The broker's write side.
///
/// While `hold` is `Some`, the broker has a packet half-written and must not
/// put anything else on the wire: a byte stream carries packets in order, so
/// injecting a PUBACK between the halves of a PUBLISH would corrupt the
/// framing rather than test it. Held bytes go out behind the packet's tail —
/// which is exactly what a sender whose peer is slow ends up doing.
struct Wire {
    writer: tokio::net::tcp::OwnedWriteHalf,
    hold: Option<Vec<u8>>,
}

type Writer = Arc<tokio::sync::Mutex<Wire>>;

async fn send(writer: &Writer, bytes: &[u8]) -> bool {
    let mut wire = writer.lock().await;
    match wire.hold.as_mut() {
        Some(held) => {
            held.extend_from_slice(bytes);
            true
        }
        None => wire.writer.write_all(bytes).await.is_ok(),
    }
}

/// Serve exactly one connection, following `script`.
///
/// The socket is split and every scripted delay runs in its own task, so the
/// broker **never stops reading**. That is what makes the stall counters mean
/// anything: a ping that arrives while the broker is stalling has to be read
/// and counted while the stall is still open, not afterwards.
async fn scripted_broker(listener: TcpListener, log: Arc<Mutex<Log>>, script: Script) {
    let Ok((socket, _)) = listener.accept().await else {
        return;
    };
    let (mut reader, writer) = socket.into_split();
    let writer: Writer = Arc::new(tokio::sync::Mutex::new(Wire { writer, hold: None }));

    // Open while the broker is deliberately withholding something.
    let stalling = Arc::new(AtomicUsize::new(0));

    loop {
        let Some((first, body)) = read_one(&mut reader).await else {
            return;
        };
        let in_stall = stalling.load(Ordering::Relaxed) == 1;

        match first >> 4 {
            // CONNECT -> CONNACK
            1 => {
                if !send(&writer, &[0x20, 0x03, 0x00, 0x00, 0x00]).await {
                    return;
                }
            }
            // SUBSCRIBE -> SUBACK, then run the script.
            8 => {
                let packet_id = [body[0], body[1]];
                if !send(
                    &writer,
                    &[0x90, 0x04, packet_id[0], packet_id[1], 0x00, 0x01],
                )
                .await
                {
                    return;
                }

                match script {
                    Script::Idle | Script::SlowPuback { .. } => {}
                    Script::SplitPublish { gap } => {
                        // Half a packet, a long silence, then the rest. The
                        // polled loop's `receive_if_ready` commits to reading
                        // the whole packet and parks here.
                        let writer = writer.clone();
                        let stalling = stalling.clone();
                        tokio::spawn(async move {
                            let packet = publish_packet("sensors/temperature", b"21");
                            let cut = packet.len() / 2;
                            {
                                let mut wire = writer.lock().await;
                                if wire.writer.write_all(&packet[..cut]).await.is_err() {
                                    return;
                                }
                                // Nothing else may reach the wire until the
                                // tail does.
                                wire.hold = Some(Vec::new());
                            }
                            stalling.store(1, Ordering::Relaxed);
                            tokio::time::sleep(gap).await;
                            stalling.store(0, Ordering::Relaxed);

                            let mut wire = writer.lock().await;
                            let held = wire.hold.take().unwrap_or_default();
                            if wire.writer.write_all(&packet[cut..]).await.is_err() {
                                return;
                            }
                            let _ = wire.writer.write_all(&held).await;
                        });
                    }
                    Script::Flood { duration } => {
                        let writer = writer.clone();
                        tokio::spawn(async move {
                            let deadline = tokio::time::Instant::now() + duration;
                            let packet = publish_packet("sensors/temperature", b"7");
                            while tokio::time::Instant::now() < deadline {
                                // The lock is taken and released per packet, so
                                // PUBACKs and PINGRESPs interleave with the
                                // flood rather than queueing behind all of it.
                                if !send(&writer, &packet).await {
                                    return;
                                }
                                tokio::task::yield_now().await;
                            }
                        });
                    }
                }
            }
            // PUBLISH from the client.
            3 => {
                let Some((topic, payload, packet_id)) = parse_publish(first, &body) else {
                    return;
                };
                {
                    let mut log = log.lock().unwrap();
                    log.publishes.push((topic, payload));
                    if in_stall {
                        log.publishes_during_stall += 1;
                    }
                }
                if let Some(id) = packet_id {
                    match script {
                        // Acknowledge late, in its own task, with the stall
                        // window open: a ping arriving meanwhile is the
                        // assertion, and the read loop has to stay live to see
                        // it.
                        Script::SlowPuback { delay } => {
                            let writer = writer.clone();
                            let stalling = stalling.clone();
                            tokio::spawn(async move {
                                stalling.store(1, Ordering::Relaxed);
                                tokio::time::sleep(delay).await;
                                stalling.store(0, Ordering::Relaxed);
                                send(&writer, &[0x40, 0x02, id[0], id[1]]).await;
                            });
                        }
                        _ => {
                            if !send(&writer, &[0x40, 0x02, id[0], id[1]]).await {
                                return;
                            }
                        }
                    }
                }
            }
            // PINGREQ -> PINGRESP
            12 => {
                {
                    let mut log = log.lock().unwrap();
                    log.pings += 1;
                    if in_stall {
                        log.pings_during_stall += 1;
                    }
                }
                if !send(&writer, &[0xD0, 0x00]).await {
                    return;
                }
            }
            14 => return,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The database under test.
// ---------------------------------------------------------------------------

/// Build an AimDb with one inbound record, and optionally an outbound record
/// that publishes every `publish_every`.
async fn build_db(
    port: u16,
    dialer: CountingDialer,
    publish: Option<(Duration, u8)>,
) -> (aimdb_core::AimDb, aimdb_core::builder::AimDbRunner) {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let connector = MqttConnector::new(format!("mqtt://127.0.0.1:{port}"))
        .transport(dialer)
        .with_client_id("session-loop");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);

    builder.configure::<u64>("temperature", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("mqtt://sensors/temperature")
            .with_deserializer(|_ctx, data: &[u8]| {
                core::str::from_utf8(data)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .ok_or_else(|| String::from("bad payload"))
            })
            .finish();
    });

    if let Some((every, qos)) = publish {
        let destination = format!("mqtt://sensors/uptime?qos={qos}");
        builder.configure::<u64>("uptime", move |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .source(move |_ctx, producer| async move {
                    let mut n = 0u64;
                    loop {
                        producer.produce(n);
                        n += 1;
                        tokio::time::sleep(every).await;
                    }
                })
                .link_to(&destination)
                .with_serializer(|_ctx, value: &u64| Ok(value.to_string().into_bytes()))
                .finish();
        });
    }

    builder.build().await.expect("build db")
}

// ---------------------------------------------------------------------------
// Criterion 1 — an idle session wakes at the ping cadence, not at 100 Hz.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_session_wakes_at_the_ping_cadence() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Log::default()));

    let dialer = CountingDialer::new();
    let sleeps = dialer.sleeps.clone();
    let (_db, runner) = build_db(port, dialer, None).await;

    // Long enough to span several of the old loop's 10 ms polls, and to cover
    // the 2 s ping cadence at least once.
    const WINDOW: Duration = Duration::from_secs(3);

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = scripted_broker(listener, log.clone(), Script::Idle) => panic!("the broker returned"),
        _ = tokio::time::sleep(WINDOW) => {}
    }

    let woke = sleeps.load(Ordering::Relaxed);
    let pings = log.lock().unwrap().pings;

    assert!(pings >= 1, "the session must still ping; saw {pings}");
    // The old loop slept `poll_interval` (10 ms) every turn: ~300 wakes in this
    // window, plus a 1 kHz burst per acknowledgement. The new one arms a timer
    // per deadline — ping, liveness, stabilisation — so a generous ceiling is
    // still two orders of magnitude below the poll.
    assert!(
        woke < 30,
        "an idle session woke {woke} times in {WINDOW:?}; the polled loop it \
         replaces would have woken ~{}",
        WINDOW.as_millis() / 10
    );
}

// ---------------------------------------------------------------------------
// Criterion 2 — a partial packet stops neither pings nor publishes.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_packet_stops_neither_pings_nor_publishes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Log::default()));

    // Longer than the 2 s ping interval, so a ping falls due while the packet
    // is half-delivered — the case the polled loop wedges on.
    const GAP: Duration = Duration::from_millis(2_600);

    let dialer = CountingDialer::new();
    let (db, runner) = build_db(port, dialer, Some((Duration::from_millis(100), 0))).await;
    let mut inbound = db
        .consumer::<u64>("temperature")
        .expect("temperature consumer")
        .subscribe();

    let received = tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = scripted_broker(listener, log.clone(), Script::SplitPublish { gap: GAP }) => {
            panic!("the broker returned")
        }
        received = async { inbound.recv().await.expect("inbound record") } => received,
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let log = log.lock().unwrap();
            panic!(
                "watchdog: {} pings ({} mid-stall), {} publishes ({} mid-stall)",
                log.pings, log.pings_during_stall, log.publishes.len(), log.publishes_during_stall
            );
        }
    };

    let log = log.lock().unwrap();
    assert!(
        log.pings_during_stall >= 1,
        "a ping must go out while a packet is half-delivered; saw {} of {} total",
        log.pings_during_stall,
        log.pings
    );
    assert!(
        log.publishes_during_stall >= 1,
        "publishes must keep flowing while a packet is half-delivered; saw {} of {} total",
        log.publishes_during_stall,
        log.publishes.len()
    );
    assert_eq!(
        received, 21,
        "the packet must still be delivered once its tail arrives"
    );
}

// ---------------------------------------------------------------------------
// Criterion 4 — a QoS 1 publish survives a slow broker without blocking pings.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_puback_does_not_block_the_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Log::default()));

    // Again longer than the ping interval: the old loop waited for this PUBACK
    // inline, at 1 kHz, with the ping behind it.
    const ACK_DELAY: Duration = Duration::from_millis(2_600);

    let dialer = CountingDialer::new();
    let (_db, runner) = build_db(port, dialer, Some((Duration::from_millis(100), 1))).await;

    let until_ping_during_stall = async {
        loop {
            if log.lock().unwrap().pings_during_stall >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = scripted_broker(listener, log.clone(), Script::SlowPuback { delay: ACK_DELAY }) => {
            panic!("the broker returned")
        }
        _ = until_ping_during_stall => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let log = log.lock().unwrap();
            panic!("watchdog: {} pings, {} publishes", log.pings, log.publishes.len());
        }
    }

    let log = log.lock().unwrap();
    assert!(
        !log.publishes.is_empty(),
        "the publish under acknowledgement must have reached the broker"
    );
    assert!(
        log.pings_during_stall >= 1,
        "the ping must go out while a QoS 1 publish waits for its PUBACK"
    );
}

// ---------------------------------------------------------------------------
// Criterion 11 — neither direction starves the other.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbound_keeps_moving_under_an_inbound_flood() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Log::default()));

    const FLOOD: Duration = Duration::from_secs(2);

    let dialer = CountingDialer::new();
    // Produce far faster than the flood's own cadence, so the outbound path is
    // saturated too and the two are genuinely competing.
    let (db, runner) = build_db(port, dialer, Some((Duration::from_millis(2), 1))).await;
    let mut inbound = db
        .consumer::<u64>("temperature")
        .expect("temperature consumer")
        .subscribe();

    let delivered = Arc::new(AtomicUsize::new(0));
    let counting = {
        let delivered = delivered.clone();
        async move {
            while inbound.recv().await.is_ok() {
                delivered.fetch_add(1, Ordering::Relaxed);
            }
        }
    };

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = scripted_broker(listener, log.clone(), Script::Flood { duration: FLOOD }) => {
            panic!("the broker returned")
        }
        _ = counting => panic!("the inbound record closed"),
        _ = tokio::time::sleep(FLOOD + Duration::from_secs(1)) => {}
    }

    let log = log.lock().unwrap();
    assert!(
        log.publishes.len() >= 50,
        "outbound starved under an inbound flood: only {} publishes got through",
        log.publishes.len()
    );
    assert!(
        delivered.load(Ordering::Relaxed) >= 10,
        "inbound starved: only {} messages were delivered",
        delivered.load(Ordering::Relaxed)
    );
}
