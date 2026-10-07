//! Allocations per MQTT round trip, per backend (`_test-backend-parity`).
//!
//! One round trip: produce → PUBLISH QoS 1 → the broker's PUBACK and echo →
//! inbound dispatch → the client's PUBACK → reader `recv`. The database, its
//! connector and the produce/recv loop run on one thread with a current-thread
//! runtime; a counting allocator counts only on that thread, so the broker on
//! another thread is not measured.
//!
//! The embedded backend's one remaining copy per message is topic and payload
//! from `OutboundRoutes`' scratch into the encoded frame; it allocates nothing.
//! The native backend's count is `rumqttc`'s: `AsyncClient::publish` takes an
//! owned topic and payload, and builds its own request.
#![cfg(feature = "_test-backend-parity")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpListener;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{ConnectorBuilder, SerializeError};
use aimdb_core::AimDbBuilder;
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::net::TokioNet;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

mod common;
use common::echo_broker;

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

struct HostClock;
impl embassy_time_driver::Driver for HostClock {
    fn now(&self) -> u64 {
        use std::sync::OnceLock;
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
// Counting allocator: per-thread counters, so tests may run in parallel.
// ---------------------------------------------------------------------------

struct Counting;

thread_local! {
    /// Set on the database thread; nothing else is counted.
    static COUNT_HERE: Cell<bool> = const { Cell::new(false) };
    /// Set only during the measured round trips.
    static WINDOW: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    /// Bytes this thread allocated and has not freed.
    static LIVE: Cell<usize> = const { Cell::new(0) };
}

fn add(cell: &'static std::thread::LocalKey<Cell<usize>>, n: usize) {
    cell.with(|c| c.set(c.get() + n));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_HERE.with(Cell::get) {
            add(&LIVE, layout.size());
            if WINDOW.with(Cell::get) {
                add(&ALLOCS, 1);
                add(&BYTES, layout.size());
            }
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNT_HERE.with(Cell::get) {
            LIVE.with(|c| c.set(c.get().saturating_sub(layout.size())));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ---------------------------------------------------------------------------
// The round trip.
// ---------------------------------------------------------------------------

const WARMUP: u64 = 100;
const MEASURED: u64 = 300;
const TOPIC: &str = "mqtt://rt/ping";

struct Report {
    allocs: usize,
    bytes: usize,
    live_before: usize,
    median: Duration,
    min: Duration,
    max: Duration,
}

/// Round trips through `connector` on a fresh thread; `COUNT_HERE` is on for
/// that thread only.
fn measure(connector: impl ConnectorBuilder + 'static) -> Report {
    std::thread::spawn(move || {
        COUNT_HERE.with(|c| c.set(true));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(round_trips(connector))
    })
    .join()
    .expect("database thread")
}

async fn round_trips(connector: impl ConnectorBuilder + 'static) -> Report {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);
    builder.configure::<u64>("ping", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to(TOPIC)
            .with_serializer(|_ctx, v: &u64| Ok(v.to_le_bytes().to_vec()))
            .with_serializer_into(8, |_ctx, v: &u64, out| {
                out.get_mut(..8)
                    .ok_or(SerializeError::BufferTooSmall)?
                    .copy_from_slice(&v.to_le_bytes());
                Ok(8)
            })
            .finish();
    });
    builder.configure::<u64>("pong", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from(TOPIC)
            .with_deserializer(|_ctx, data: &[u8]| {
                data.try_into()
                    .map(u64::from_le_bytes)
                    .map_err(|_| String::from("not 8 bytes"))
            })
            .finish();
    });
    let (db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());
    let producer = db.producer::<u64>("ping").expect("producer");
    let mut pong = db.subscribe::<u64>("pong").expect("subscribe");

    // Warm-up also waits out the connect and subscribe.
    tokio::time::timeout(Duration::from_secs(60), async {
        for n in 0..WARMUP {
            round_trip(&producer, &mut pong, n).await;
        }
    })
    .await
    .expect("warm-up round trips");

    let mut latencies = Vec::with_capacity(MEASURED as usize);
    let live_before = LIVE.with(Cell::get);
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|c| c.set(0));
    WINDOW.with(|w| w.set(true));
    for n in WARMUP..WARMUP + MEASURED {
        let start = Instant::now();
        round_trip(&producer, &mut pong, n).await;
        latencies.push(start.elapsed());
    }
    WINDOW.with(|w| w.set(false));

    latencies.sort();
    Report {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        live_before,
        median: latencies[latencies.len() / 2],
        min: latencies[0],
        max: latencies[latencies.len() - 1],
    }
}

/// Produce `n` and wait until it comes back through the broker.
async fn round_trip(
    producer: &aimdb_core::Producer<u64>,
    pong: &mut aimdb_core::buffer::Reader<u64>,
    n: u64,
) {
    producer.produce(n);
    while pong.recv().await.expect("pong open") != n {}
}

/// An echo broker on its own thread; returns its port.
fn broker() -> u16 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            echo_broker(listener).await;
        });
    });
    rx.recv().expect("broker port")
}

fn print(backend: &str, r: &Report) {
    println!(
        "{backend:<8} {:>6.2} allocs/round trip {:>8.1} bytes/round trip  live heap before: {} B  latency median {:?} ({:?}–{:?})",
        r.allocs as f64 / MEASURED as f64,
        r.bytes as f64 / MEASURED as f64,
        r.live_before,
        r.median,
        r.min,
        r.max
    );
}

/// The embedded backend allocates nothing per round trip.
#[test]
fn the_embedded_backend_allocates_nothing_per_round_trip() {
    let port = broker();
    let report =
        measure(MqttConnector::new(format!("mqtt://127.0.0.1:{port}")).transport(TokioNet::tcp()));
    print("embedded", &report);
    assert_eq!(
        report.allocs, 0,
        "{} allocations in {MEASURED} round trips",
        report.allocs
    );
}

/// The native backend's allocations are `rumqttc`'s; the bound is what the
/// prototype measured.
#[test]
fn the_native_backend_stays_within_rumqttcs_allocations() {
    let port = broker();
    let report = measure(MqttConnector::new(format!("mqtt://127.0.0.1:{port}")));
    print("native", &report);
    let per_round_trip = report.allocs as f64 / MEASURED as f64;
    assert!(
        per_round_trip <= 11.0,
        "{per_round_trip:.2} allocations per round trip"
    );
}
