//! `tests/session_loop.rs`'s promises, re-driven over a real TLS 1.3 session
//! against a pinned self-signed root.
//!
//! Also where `DuplexHandle`'s disjointness is exercised for real: while the
//! read half is parked inside `TlsReader`, the write half has to push pings
//! through `TlsWriter`. An `embedded-tls` whose reader took the write lock would
//! hang these tests rather than fail quietly in the field.
#![cfg(feature = "_test-tls-broker")]

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::SeedableRng as _;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

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

/// Real wall-clock time for `embassy-time`, which this binary links.
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

/// The name the certificate is issued for, and the name the client verifies.
const BROKER_HOST: &str = "localhost";

/// A self-signed certificate for `localhost`, as (server chain, key, root CA).
fn self_signed() -> (
    CertificateDer<'static>,
    PrivateKeyDer<'static>,
    &'static [u8],
) {
    let cert = rcgen::generate_simple_self_signed(vec![BROKER_HOST.to_string()])
        .expect("generate self-signed certificate");
    let der = cert.cert.der().to_vec();
    let key = PrivateKeyDer::try_from(cert.key_pair.serialize_der()).expect("server key");
    let ca: &'static [u8] = Box::leak(der.clone().into_boxed_slice());
    (CertificateDer::from(der), key, ca)
}

/// Accept one TLS connection and run the scripted broker over it.
async fn serve_one_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    log: Arc<Mutex<Log>>,
    script: Script,
) {
    let Ok((socket, _)) = listener.accept().await else {
        return;
    };
    let Ok(stream) = acceptor.accept(socket).await else {
        return;
    };
    scripted_broker(stream, log, script).await;
}

/// Everything a `mqtts://` test needs: a listener, its acceptor, and the
/// connector's TLS materials.
fn tls_setup() -> (
    TcpListener,
    TlsAcceptor,
    aimdb_mqtt_connector::TlsOptions,
    u16,
) {
    let (chain, key, ca_der) = self_signed();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![chain], key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().unwrap().port();
    let listener = TcpListener::from_std(listener).expect("adopt listener");

    // On a board these are `StaticCell`s; here one leak apiece.
    let rng: &'static mut (dyn embedded_tls::CryptoRngCore + Send) =
        Box::leak(Box::new(rand::rngs::StdRng::from_entropy()));
    let read_buf: &'static mut [u8] = Box::leak(vec![0u8; 16_640].into_boxed_slice());
    let write_buf: &'static mut [u8] = Box::leak(vec![0u8; 4_096].into_boxed_slice());

    (
        listener,
        acceptor,
        aimdb_mqtt_connector::TlsOptions::new(rng, ca_der, read_buf, write_buf),
        port,
    )
}

/// An AimDb whose MQTT connector speaks `mqtts://` through `dialer`, with one
/// inbound record and optionally an outbound one publishing every `every` at
/// `qos`.
async fn build_tls_db(
    port: u16,
    dialer: CountingDialer,
    options: aimdb_mqtt_connector::TlsOptions,
    publish: Option<(Duration, u8)>,
) -> (aimdb_core::AimDb, aimdb_core::builder::AimDbRunner) {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt};
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let connector = MqttConnector::new(format!("mqtts://{BROKER_HOST}:{port}"))
        .tls(dialer, options)
        .with_client_id("tls-session");

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
                .link_to("mqtt://sensors/uptime")
                .with_qos(qos)
                .with_serializer(|_ctx, value: &u64| Ok(value.to_string().into_bytes()))
                .finish();
        });
    }

    builder.build().await.expect("build db")
}

// ---------------------------------------------------------------------------
// Criterion 1, over TLS.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_tls_session_wakes_at_the_ping_cadence() {
    let (listener, acceptor, options, port) = tls_setup();
    let log = Arc::new(Mutex::new(Log::default()));

    let dialer = CountingDialer::new();
    let sleeps = dialer.sleeps();
    let (_db, runner) = build_tls_db(port, dialer, options, None).await;

    const WINDOW: Duration = Duration::from_secs(3);

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = serve_one_tls(listener, acceptor, log.clone(), Script::Idle) => {
            panic!("the broker returned")
        }
        _ = tokio::time::sleep(WINDOW) => {}
    }

    let woke = sleeps.load(Ordering::Relaxed);
    let pings = log.lock().unwrap().pings;

    assert!(pings >= 1, "the TLS session must still ping; saw {pings}");
    assert!(
        woke < 30,
        "an idle TLS session woke {woke} times in {WINDOW:?}; the polled loop \
         it replaces would have woken ~{}",
        WINDOW.as_millis() / 10
    );
}

// ---------------------------------------------------------------------------
// Criterion 2, over TLS.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_packet_over_tls_stops_neither_pings_nor_publishes() {
    let (listener, acceptor, options, port) = tls_setup();
    let log = Arc::new(Mutex::new(Log::default()));

    // Longer than the 2 s ping interval, so a ping falls due while the MQTT
    // packet is half-delivered — here, half of it inside a complete TLS record
    // and the rest in a later one.
    const GAP: Duration = Duration::from_millis(2_600);

    let dialer = CountingDialer::new();
    let (db, runner) =
        build_tls_db(port, dialer, options, Some((Duration::from_millis(100), 0))).await;
    let mut inbound = db
        .consumer::<u64>("temperature")
        .expect("temperature consumer")
        .subscribe();

    let received = tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = serve_one_tls(listener, acceptor, log.clone(), Script::SplitPublish { gap: GAP }) => {
            panic!("the broker returned")
        }
        received = async { inbound.recv().await.expect("inbound record") } => received,
        _ = tokio::time::sleep(Duration::from_secs(60)) => {
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
        "a ping must go out over TLS while a packet is half-delivered; saw {} of {}",
        log.pings_during_stall,
        log.pings
    );
    assert!(
        log.publishes_during_stall >= 1,
        "publishes must keep flowing over TLS meanwhile; saw {} of {}",
        log.publishes_during_stall,
        log.publishes.len()
    );
    assert_eq!(
        received, 21,
        "the packet must still be delivered once its tail arrives"
    );
}

// ---------------------------------------------------------------------------
// Criterion 4 over TLS — and criterion 9's concurrent read and write.
// ---------------------------------------------------------------------------

/// A QoS 1 publish waiting on a slow broker must not stop the ping — which over
/// TLS means `TlsWriter` taking the write lock while `TlsReader` is parked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_puback_over_tls_does_not_block_the_ping() {
    let (listener, acceptor, options, port) = tls_setup();
    let log = Arc::new(Mutex::new(Log::default()));

    const ACK_DELAY: Duration = Duration::from_millis(2_600);

    let dialer = CountingDialer::new();
    let (_db, runner) =
        build_tls_db(port, dialer, options, Some((Duration::from_millis(100), 1))).await;

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
        _ = serve_one_tls(listener, acceptor, log.clone(), Script::SlowPuback { delay: ACK_DELAY }) => {
            panic!("the broker returned")
        }
        _ = until_ping_during_stall => {}
        _ = tokio::time::sleep(Duration::from_secs(60)) => {
            let log = log.lock().unwrap();
            panic!(
                "watchdog: {} pings, {} publishes — a concurrent TLS read and \
                 write did not complete",
                log.pings,
                log.publishes.len()
            );
        }
    }

    let log = log.lock().unwrap();
    assert!(
        !log.publishes.is_empty(),
        "the publish under acknowledgement must have reached the broker"
    );
    assert!(
        log.pings_during_stall >= 1,
        "the ping must go out over TLS while a QoS 1 publish waits for its PUBACK"
    );
}
