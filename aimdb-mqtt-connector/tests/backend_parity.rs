//! Both backends against the same broker, in one process
//! (`_test-backend-parity`).
//!
//! `Native` is `rumqttc` over MQTT 3.1.1; `Embedded` is `mountain-mqtt` over
//! MQTT 5 and `TokioNet::tcp()`. The point is that the two are interchangeable
//! from a record's point of view: same link URLs, same payloads on the wire.
#![cfg(feature = "_test-backend-parity")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::net::TokioNet;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

mod common;
use common::{fake_broker_concurrent, Seen};

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
// Nothing else defines `_defmt_timestamp` now that the connector pulls no
// crate enabling `embassy-time/defmt-timestamp-uptime`.
defmt::timestamp!("{=u64:us}", 0);

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

const INBOUND: &str = "mqtt://parity/inbound";
const OUTBOUND: &str = "mqtt://parity/outbound";

/// One database with one inbound and one outbound record, so both backends are
/// exercised through identical registrations.
fn build_db(
    connector: impl aimdb_core::ConnectorBuilder + 'static,
    value: u64,
) -> impl std::future::Future<Output = (aimdb_core::AimDb, aimdb_core::builder::AimDbRunner)> {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);

    builder.configure::<u64>("inbound", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from(INBOUND)
            .with_deserializer(|_ctx, data: &[u8]| {
                core::str::from_utf8(data)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .ok_or_else(|| String::from("bad payload"))
            })
            .finish();
    });

    builder.configure::<u64>("outbound", move |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(move |_ctx, producer| async move {
                loop {
                    producer.produce(value);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .link_to(OUTBOUND)
            .with_serializer(|_ctx, v: &u64| Ok(v.to_string().into_bytes()))
            .finish();
    });

    async move { builder.build().await.expect("build db") }
}

/// Both backends complete a session against the same broker at the same time,
/// and a record round-trips through each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_backends_round_trip_against_one_broker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("mqtt://127.0.0.1:{port}");
    let seen = Arc::new(Mutex::new(Seen::default()));

    // One `new` whichever backends are compiled in: the transport, or its
    // absence, picks the backend.
    let native = MqttConnector::new(url.clone()).with_client_id("parity-native");
    let embedded = MqttConnector::new(url)
        .transport(TokioNet::tcp())
        .with_client_id("parity-embedded");

    let (native_db, native_runner) = build_db(native, 1).await;
    let (embedded_db, embedded_runner) = build_db(embedded, 2).await;

    let mut native_in = native_db
        .consumer::<u64>("inbound")
        .expect("native consumer")
        .subscribe();
    let mut embedded_in = embedded_db
        .consumer::<u64>("inbound")
        .expect("embedded consumer")
        .subscribe();

    let broker = fake_broker_concurrent(listener, seen.clone(), Some(("parity/inbound", b"7")));
    let seen_for_wait = seen.clone();

    let (native_value, embedded_value) = tokio::select! {
        _ = native_runner.run() => panic!("the native runner returned"),
        _ = embedded_runner.run() => panic!("the embedded runner returned"),
        _ = broker => panic!("the broker returned"),
        values = async {
            let values = (
                native_in.recv().await.expect("native inbound"),
                embedded_in.recv().await.expect("embedded inbound"),
            );
            // Both outbound links must land before the assertions below.
            while seen_for_wait.lock().unwrap().published.len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            values
        } => values,
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: {} connects, {:?} subscribed, {} published",
                seen.connects,
                seen.subscribed_topics(),
                seen.published.len()
            );
        }
    };

    assert_eq!(native_value, 7, "the broker's PUBLISH must reach Native");
    assert_eq!(
        embedded_value, 7,
        "the broker's PUBLISH must reach Embedded"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.connects, 2, "both backends must connect");
    assert_eq!(
        seen.subscribed_topics()
            .iter()
            .filter(|t| **t == "parity/inbound")
            .count(),
        2,
        "both backends must subscribe the inbound topic"
    );

    // Same record, same serializer, same bytes — whichever backend carried it.
    let mut payloads: Vec<&[u8]> = seen
        .published
        .iter()
        .filter(|(topic, _)| topic == "parity/outbound")
        .map(|(_, payload)| payload.as_slice())
        .collect();
    payloads.sort_unstable();
    payloads.dedup();
    assert_eq!(
        payloads,
        vec![b"1".as_slice(), b"2".as_slice()],
        "each backend must publish its own record's bytes"
    );
}

/// `with_credentials` reaches the wire on both backends.
///
/// It is new plumbing on `Native` — `rumqttc` previously took credentials only
/// from the URL authority — so a setter that was accepted and dropped would
/// look exactly like success.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_credentials_reaches_the_wire_on_both_backends() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("mqtt://127.0.0.1:{port}");
    let seen = Arc::new(Mutex::new(Seen::default()));

    let native = MqttConnector::new(url.clone())
        .with_client_id("creds-native")
        .with_credentials("hub", "s3cret");
    let embedded = MqttConnector::new(url)
        .transport(TokioNet::tcp())
        .with_client_id("creds-embedded")
        .with_credentials("hub", "s3cret");

    let (_native_db, native_runner) = build_db(native, 1).await;
    let (_embedded_db, embedded_runner) = build_db(embedded, 2).await;

    let broker = fake_broker_concurrent(listener, seen.clone(), None);
    let seen_for_wait = seen.clone();

    tokio::select! {
        _ = native_runner.run() => panic!("the native runner returned"),
        _ = embedded_runner.run() => panic!("the embedded runner returned"),
        _ = broker => panic!("the broker returned"),
        _ = async {
            while seen_for_wait.lock().unwrap().credentials.len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            panic!("watchdog: saw {:?}", seen.lock().unwrap().credentials);
        }
    }

    let seen = seen.lock().unwrap();
    let expected = Some((String::from("hub"), String::from("s3cret")));
    for (n, credentials) in seen.credentials.iter().enumerate() {
        assert_eq!(
            *credentials, expected,
            "connection {n} ({}) dropped the credentials",
            seen.client_ids[n]
        );
    }
}

/// A **hostname** is a broker address on both backends.
///
/// The embedded backend used to vet plain `mqtt://` hosts with
/// `Ipv4Addr::from_str` and reject everything else, so `.transport(..)` — the
/// call that is supposed to leave behaviour unchanged — was the difference
/// between a URL that works and one that does not. Resolving `host` is the
/// dialer's job on every adapter, so the gate is gone and the two agree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hostname_is_a_broker_address_on_both_backends() {
    // Bound by name, so the address the broker listens on is whichever one
    // `localhost` resolves to first here — the same one the dialers get.
    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("mqtt://localhost:{port}");
    let seen = Arc::new(Mutex::new(Seen::default()));

    let native = MqttConnector::new(url.clone()).with_client_id("host-native");
    let embedded = MqttConnector::new(url)
        .transport(TokioNet::tcp())
        .with_client_id("host-embedded");

    let (_native_db, native_runner) = build_db(native, 1).await;
    let (_embedded_db, embedded_runner) = build_db(embedded, 2).await;

    let broker = fake_broker_concurrent(listener, seen.clone(), None);
    let seen_for_wait = seen.clone();

    tokio::select! {
        _ = native_runner.run() => panic!("the native runner returned"),
        _ = embedded_runner.run() => panic!("the embedded runner returned"),
        _ = broker => panic!("the broker returned"),
        _ = async {
            while seen_for_wait.lock().unwrap().connects < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: only {} of 2 backends connected by name ({:?})",
                seen.connects, seen.client_ids
            );
        }
    }

    let seen = seen.lock().unwrap();
    let mut ids = seen.client_ids.clone();
    ids.sort();
    assert_eq!(
        ids,
        vec![String::from("host-embedded"), String::from("host-native")],
        "both backends must reach the broker through a hostname"
    );
}
