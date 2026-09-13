//! Liveness under a stalled peer, pings that keep flowing while a QoS 1 publish
//! is outstanding, and an idle session that wakes at the ping cadence.
//!
//! The broker is scripted rather than `common::fake_broker`, because each test
//! controls *when* it answers — mid-packet, late, or not at all.
#![cfg(feature = "_test-tokio-broker")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;

mod common;
use common::{scripted_broker, CountingDialer, Log, Script, SCRIPT_TOPIC};

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
// A scripted broker: the same wire format as `common`, but the test decides
// when each answer goes out.
// ---------------------------------------------------------------------------

/// Accept one plain-TCP connection and serve it under `script`.
async fn serve_one(listener: TcpListener, log: Arc<Mutex<Log>>, script: Script) {
    let Ok((socket, _)) = listener.accept().await else {
        return;
    };
    scripted_broker(socket, log, script).await;
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
            .link_from(&format!("mqtt://{SCRIPT_TOPIC}"))
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
    let sleeps = dialer.sleeps();
    let (_db, runner) = build_db(port, dialer, None).await;

    // Long enough to span several of the old loop's 10 ms polls, and to cover
    // the 2 s ping cadence at least once.
    const WINDOW: Duration = Duration::from_secs(3);

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = serve_one(listener, log.clone(), Script::Idle) => panic!("the broker returned"),
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
        _ = serve_one(listener, log.clone(), Script::SplitPublish { gap: GAP }) => {
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
        _ = serve_one(listener, log.clone(), Script::SlowPuback { delay: ACK_DELAY }) => {
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
        _ = serve_one(listener, log.clone(), Script::Flood { duration: FLOOD }) => {
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
