//! Host smoke for the embedded MQTT backend over `TokioNet::tcp()`
//! (`_test-tokio-broker`).
//!
//! The same session loop the Embassy smoke drives, but over a real TCP socket
//! and a fake broker on the same host — no network stack to stand up. What it
//! adds over that smoke is the reconnect: the broker hangs up after the first
//! SUBACK, and the loop must dial again and re-subscribe.
#![cfg(feature = "_test-tokio-broker")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;

mod common;
use common::{fake_broker, Seen};

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
// This binary must define `_defmt_timestamp` itself. `embassy-time` would —
// `defmt-timestamp-uptime` is enabled here, as it is for `embassy_broker` —
// but nothing in this test references `embassy-time`, so its object never
// reaches the link and the symbol would be undefined. `embassy_broker` pulls
// it in through `embassy-net` and therefore must *not* define one.
defmt::timestamp!("{=u64:us}", 0);

/// Real wall-clock time; the session loop's delays are `embassy_time`'s until
/// it takes core's `Delay`.
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
// The test.
// ---------------------------------------------------------------------------

/// The session loop re-subscribes after the broker hangs up.
///
/// Losing that is silent: publishes keep working and inbound routing simply
/// stops, so this is the assertion the reconnect loop exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_session_loop_reconnects_and_resubscribes() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::net::TokioNet;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));

    let connector = MqttConnector::new(format!("mqtt://127.0.0.1:{port}"))
        .transport(TokioNet::tcp())
        .with_client_id("host-smoke");

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
    let (_db, runner) = builder.build().await.expect("build db");

    let broker = fake_broker(listener, seen.clone(), 1, None);
    let until_resubscribed = async {
        while seen.lock().unwrap().subscribes.len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = broker => panic!("the broker returned"),
        _ = until_resubscribed => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: {} connects, {} subscribes",
                seen.connects,
                seen.subscribes.len()
            );
        }
    }

    let seen = seen.lock().unwrap();
    assert!(
        seen.connects >= 2,
        "the loop must redial after the hang-up; saw {} connects",
        seen.connects
    );
    for (n, topics) in seen.subscribes.iter().enumerate() {
        assert!(
            topics.iter().any(|t| t == "sensors/temperature"),
            "connection {n} did not subscribe the inbound topic; saw {topics:?}"
        );
    }
}

/// The embedded backend carries records both ways over `TokioNet::tcp()`, on a
/// multi-thread runtime: an inbound PUBLISH reaches a record, and a record's
/// outbound link reaches the broker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_embedded_backend_round_trips_records_on_a_multi_thread_runtime() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::net::TokioNet;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));

    let connector = MqttConnector::new(format!("mqtt://127.0.0.1:{port}"))
        .transport(TokioNet::tcp())
        .with_client_id("round-trip");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);

    // Inbound: the broker's PUBLISH lands here.
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

    // Outbound: this record's producer publishes to the broker.
    builder.configure::<u64>("uptime", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(|_ctx, producer| async move {
                loop {
                    producer.produce(42u64);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .link_to("mqtt://sensors/uptime")
            .with_serializer(|_ctx, v: &u64| Ok(v.to_string().into_bytes()))
            .finish();
    });

    let (db, runner) = builder.build().await.expect("build db");
    let mut inbound = db
        .consumer::<u64>("temperature")
        .expect("temperature consumer")
        .subscribe();

    let broker = fake_broker(
        listener,
        seen.clone(),
        0,
        Some(("sensors/temperature", b"23")),
    );
    let seen_for_wait = seen.clone();

    let received = tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = broker => panic!("the broker returned"),
        received = async {
            let value = inbound.recv().await.expect("inbound record");
            while seen_for_wait.lock().unwrap().published.is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            value
        } => received,
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: {} connects, {} subscribes, {} publishes",
                seen.connects,
                seen.subscribes.len(),
                seen.published.len()
            );
        }
    };

    assert_eq!(received, 23, "the broker's PUBLISH must reach the record");

    let seen = seen.lock().unwrap();
    let (topic, payload) = seen
        .published
        .first()
        .expect("the outbound link must reach the broker");
    assert_eq!(topic, "sensors/uptime");
    assert_eq!(payload, b"42", "the serializer's bytes must arrive intact");
}

/// Two connectors in one process keep their own identities.
///
/// They shared a process-global cell before the channels moved to `Arc`, so the
/// second silently connected under the first's client id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_connectors_in_one_process_keep_their_own_client_ids() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::net::TokioNet;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("mqtt://127.0.0.1:{port}");
    let seen = Arc::new(Mutex::new(Seen::default()));

    let mut runners = Vec::new();
    for id in ["first-node", "second-node"] {
        let mut builder = AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(
                MqttConnector::new(url.clone())
                    .transport(TokioNet::tcp())
                    .with_client_id(id),
            );
        builder.configure::<u64>("temperature", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from("mqtt://sensors/temperature")
                .with_deserializer(|_ctx, _data: &[u8]| Ok(0u64))
                .finish();
        });
        let (_db, runner) = builder.build().await.expect("build db");
        runners.push(runner);
    }

    let broker = common::fake_broker_concurrent(listener, seen.clone(), None);
    let seen_for_wait = seen.clone();
    let second = runners.pop().unwrap();
    let first = runners.pop().unwrap();

    tokio::select! {
        _ = first.run() => panic!("the first runner returned"),
        _ = second.run() => panic!("the second runner returned"),
        _ = broker => panic!("the broker returned"),
        _ = async {
            while seen_for_wait.lock().unwrap().client_ids.len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            panic!("watchdog: saw {:?}", seen.lock().unwrap().client_ids);
        }
    }

    let mut ids = seen.lock().unwrap().client_ids.clone();
    ids.sort();
    assert_eq!(ids, vec!["first-node", "second-node"]);
}
