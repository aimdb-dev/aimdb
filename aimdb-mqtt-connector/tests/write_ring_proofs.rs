//! The embedded backend's packet-size behaviour against a fake broker
//! (`_test-tokio-broker`). A retained message over the client's Maximum
//! Packet Size is covered in `tokio_broker`.
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

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::net::TokioNet;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

/// A 3,000-byte publish never reaches the broker, the session never errors,
/// and the small publishes around it keep flowing. Nothing counts the drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proof_an_oversize_publish_is_dropped_silently() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));

    let connector = MqttConnector::new(format!("mqtt://127.0.0.1:{port}"))
        .transport(TokioNet::tcp())
        .with_client_id("proof-drop");
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);
    builder.configure::<u64>("blob", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(|_ctx, producer| async move {
                loop {
                    producer.produce(1u64); // 3,000 bytes
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    producer.produce(2u64); // 1 byte
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .link_to("mqtt://sensors/blob")
            .with_serializer(|_ctx, v: &u64| {
                Ok(if *v == 1 {
                    vec![b'x'; 3000]
                } else {
                    b"2".to_vec()
                })
            })
            .finish();
    });
    let (_db, runner) = builder.build().await.expect("build db");
    let broker = fake_broker(listener, seen.clone(), 0, None);

    let until_small_ones = async {
        while seen.lock().unwrap().published.len() < 5 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = broker => panic!("the broker returned"),
        _ = until_small_ones => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => panic!("watchdog"),
    }

    let seen = seen.lock().unwrap();
    assert!(
        seen.published.iter().all(|(_, p)| p == b"2"),
        "only the 1-byte payloads reached the broker"
    );
    assert_eq!(seen.connects, 1, "the session never errored");
}
