//! B1-Outbound — time per message when the transport parks before every
//! value. Informational: not part of `bench-gate`, since timings need a quiet
//! host.
//!
//! One busy route among N (1, 8, 64, 256), the producer and the transport in
//! separate tasks on a current-thread Tokio runtime. The producer writes one
//! value and waits until it was pulled, so every message pays one park and
//! one wake-up of the transport. Two columns:
//!
//! - **task per route:** one task per route awaiting `Reader::recv`, the
//!   shape of the per-route publishers `OutboundRoutes` replaced.
//! - **OutboundRoutes:** one task pulling with `OutboundRoutes::next`, which
//!   polls only the routes that woke.
//!
//! A per-route scan would show as `OutboundRoutes` growing with N. Compare
//! columns within one run, not across hosts.
//!
//! Run `cargo bench -p aimdb-bench --bench b1_outbound_wakeup`.

use std::future::poll_fn;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{ConnectorBuilder, SerializeError};
use aimdb_core::{AimDb, AimDbBuilder, DbResult, OutboundRoutes, Producer, StringKey};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use futures::task::AtomicWaker;

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

const ROUTE_COUNTS: &[usize] = &[1, 8, 64, 256];
const WARMUP: u64 = 2_000;
const MEASURE: u64 = 20_000;
const RUNS: usize = 5;
const RING: usize = 64;
const SCHEME: &str = "bench";

/// Registers the scheme so `link_to` succeeds; drives nothing.
struct NoopConnectorBuilder;

impl ConnectorBuilder for NoopConnectorBuilder {
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        &'a self,
        _db: &'a AimDb,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = DbResult<
                        Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn scheme(&self) -> &str {
        SCHEME
    }
}

/// `n` SPMC records `r0..r{n}`, each linked to `bench://r{i}` with a scratch
/// serializer.
async fn build_db(n: usize) -> AimDb {
    let runtime = Arc::new(TokioAdapter::new().expect("tokio adapter"));
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(NoopConnectorBuilder);
    for i in 0..n {
        builder.configure::<u64>(StringKey::intern(format!("r{i}")), move |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: RING })
                .link_to(&format!("{SCHEME}://r{i}"))
                .with_serializer(|_ctx, v: &u64| Ok(v.to_le_bytes().to_vec()))
                .with_serializer_into(8, |_ctx, v: &u64, out| {
                    out.get_mut(..8)
                        .ok_or(SerializeError::BufferTooSmall)?
                        .copy_from_slice(&v.to_le_bytes());
                    Ok(8)
                })
                .finish();
        });
    }
    let (db, runner) = builder.build().await.expect("build");
    // Nothing in the runner is needed; keep it alive for the process.
    std::mem::forget(runner);
    db
}

/// Values pulled so far, and the producer's waker.
struct Ack {
    count: AtomicU64,
    waker: AtomicWaker,
}

impl Ack {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicU64::new(0),
            waker: AtomicWaker::new(),
        })
    }

    fn bump(&self) {
        self.count.fetch_add(1, Ordering::Release);
        self.waker.wake();
    }

    async fn wait_for(&self, target: u64) {
        poll_fn(|cx| {
            self.waker.register(cx.waker());
            if self.count.load(Ordering::Acquire) >= target {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// Writes one value, waits until it was pulled. Returns ns and allocations
/// per message over the measured window.
async fn drive_producer(producer: Producer<u64>, ack: Arc<Ack>) -> (f64, f64) {
    for i in 0..WARMUP {
        producer.produce(i);
        ack.wait_for(i + 1).await;
    }
    reset();
    let start = Instant::now();
    for i in WARMUP..WARMUP + MEASURE {
        producer.produce(i);
        ack.wait_for(i + 1).await;
    }
    let ns = start.elapsed().as_nanos() as f64 / MEASURE as f64;
    let (allocs, _) = snapshot();
    (ns, allocs as f64 / MEASURE as f64)
}

async fn task_per_route(n: usize) -> (f64, f64) {
    let db = build_db(n).await;
    let busy = n - 1;
    let producer = db.producer::<u64>(format!("r{busy}")).expect("producer");
    let ack = Ack::new();
    let mut tasks = Vec::new();
    for i in 0..n {
        let mut reader = db.subscribe::<u64>(format!("r{i}")).expect("subscribe");
        let ack = ack.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let v = reader.recv().await.expect("recv");
                black_box((i, v));
                ack.bump();
                if v + 1 == WARMUP + MEASURE {
                    break;
                }
            }
        }));
    }
    let result = drive_producer(producer, ack).await;
    tasks.pop().expect("busy task").await.expect("busy task");
    for task in tasks {
        task.abort();
    }
    result
}

async fn outbound_routes(n: usize) -> (f64, f64) {
    let db = build_db(n).await;
    let busy = n - 1;
    let producer = db.producer::<u64>(format!("r{busy}")).expect("producer");
    let mut outbound = OutboundRoutes::new(&db, SCHEME).expect("routes");
    let ack = Ack::new();
    let transport = {
        let ack = ack.clone();
        tokio::spawn(async move {
            loop {
                let msg = outbound.next().await.expect("route open");
                let v = u64::from_le_bytes(msg.payload.as_slice().try_into().expect("8 bytes"));
                black_box((msg.route.id, msg.topic, v));
                ack.bump();
                if v + 1 == WARMUP + MEASURE {
                    break;
                }
            }
        })
    };
    let result = drive_producer(producer, ack).await;
    transport.await.expect("transport");
    result
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Median of `RUNS` runs, each on a fresh database.
fn row<F, Fut>(runtime: &tokio::runtime::Runtime, name: &str, n: usize, run: F) -> f64
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = (f64, f64)>,
{
    let mut ns = Vec::with_capacity(RUNS);
    let mut allocs = 0.0;
    for _ in 0..RUNS {
        let (t, a) = runtime.block_on(run(n));
        ns.push(t);
        allocs = a;
    }
    let lo = ns.iter().copied().fold(f64::MAX, f64::min);
    let hi = ns.iter().copied().fold(0.0, f64::max);
    let m = median(ns);
    println!(
        "{name:<16} {n:>4} routes  {m:>7.0} ns/msg  ({lo:.0}–{hi:.0})  {allocs:.3} allocs/msg"
    );
    m
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    println!("=== B1 outbound wake-up: one busy route among N, current-thread runtime ===");
    for &n in ROUTE_COUNTS {
        row(&runtime, "task per route", n, task_per_route);
        row(&runtime, "OutboundRoutes", n, outbound_routes);
        println!();
    }
}
