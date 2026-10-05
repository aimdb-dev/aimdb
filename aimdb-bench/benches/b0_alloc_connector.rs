//! B0-Connector — per-message allocations at the connector boundary.
//!
//! Measures what AimDB's connector interfaces cost per message, independent of
//! any real transport:
//!
//! - **Inbound:** `InboundDispatch::dispatch` for an exact topic, a pattern
//!   (`{device}`, MQTT grammar), and a keyed pattern with a known and a new
//!   key.
//! - **Outbound:** `OutboundRoutes::next` with a static topic, a written topic
//!   and the owned serializer, plus eight routes that are all ready
//!   (round-robin) and one pull that parks before every value (the waker
//!   path).
//!
//! Buffers, ingest and routing allocate nothing (design 037, and the
//! `inbound_dispatch` row here); every non-zero row is a cost of the connector
//! interface or of the serializer the link chose. The
//! expected values are asserted, so a regression *or* an improvement fails the
//! bench until `EXPECTED` and the committed baseline are updated together.
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_connector`; results are
//! written to `aimdb-bench/target/bench-results/b0_alloc_connector.json` and
//! compared by hand with `aimdb-bench/data/baselines/b0_alloc_connector.json`.

use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use std::sync::Arc;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_bench::reports::{write_reports, AllocReport};
use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{ConnectorBuilder, SerializeError};
use aimdb_core::{
    AimDb, AimDbBuilder, DbResult, ExactGrammar, InboundDispatch, OutboundRoutes, StringKey,
};
use aimdb_mqtt_connector::MqttGrammar;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

const WARMUP_ITERS: usize = 100;
const MEASURE_ITERS: usize = 2_000;
const DECOY_ROUTES: usize = 63;
const SCRATCH_CAPACITY: usize = 64;
/// Room for a new key on every warm-up and measured message.
const KEY_CAPACITY: u16 = 4096;

/// Allocations per message. Update together with
/// `data/baselines/b0_alloc_connector.json`.
const EXPECTED: &[(&str, u64)] = &[
    ("inbound_dispatch", 0),
    ("inbound_dispatch_pattern", 0),
    ("inbound_dispatch_keyed_known", 0),
    ("inbound_dispatch_keyed_new", 1),
    ("outbound_next_static_topic", 0),
    ("outbound_next_written_topic", 0),
    ("outbound_next_owned", 1),
    ("outbound_next_round_robin", 0),
    ("outbound_next_parked", 0),
];

/// Routes in the `outbound_next_round_robin` row.
const ROUND_ROBIN_ROUTES: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Reading {
    id: u32,
    value: f32,
}

fn reading(i: usize) -> Reading {
    Reading {
        id: i as u32,
        value: 21.5,
    }
}

fn encode(r: &Reading) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&r.id.to_le_bytes());
    out[4..].copy_from_slice(&r.value.to_le_bytes());
    out
}

// --- A connector that owns no transport: it only registers the scheme. ----

const SCHEME: &str = "bench";

struct NoopConnectorBuilder;

impl ConnectorBuilder for NoopConnectorBuilder {
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        &'a self,
        _db: &'a AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn scheme(&self) -> &str {
        SCHEME
    }
}

async fn build_db(configure: impl FnOnce(&mut AimDbBuilder)) -> AimDb {
    let runtime = Arc::new(TokioAdapter::new().expect("tokio adapter"));
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(NoopConnectorBuilder);
    configure(&mut builder);
    let (db, _runner) = builder.build().await.expect("build");
    db
}

// --- Inbound ----------------------------------------------------------------

/// `DECOY_ROUTES` exact links, so routing scans a realistic table.
fn add_decoys(b: &mut AimDbBuilder) {
    for i in 0..DECOY_ROUTES {
        let topic = format!("bench://in/decoy/{i}");
        b.configure::<Reading>(StringKey::intern(format!("in.decoy{i}")), |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from(&topic)
                .with_deserializer(|_ctx, _bytes| Ok(reading(0)))
                .finish();
        });
    }
}

/// One linked record plus the decoys.
async fn inbound_db() -> AimDb {
    build_db(|b| {
        b.configure::<Reading>("in.target", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
                .link_from("bench://in/target")
                .with_deserializer(|_ctx, bytes| Ok(reading(bytes[0] as usize)))
                .finish();
        });
        add_decoys(b);
    })
    .await
}

/// One record on `in/{device}/target`, keyed or not, plus the decoys.
async fn pattern_db(keyed: bool) -> AimDb {
    build_db(|b| {
        b.configure::<Reading>("in.pattern", move |reg| {
            let link = reg
                .buffer(BufferCfg::SpmcRing { capacity: 64 })
                .link_from("bench://in/{device}/target");
            let link = if keyed {
                link.key("device", KEY_CAPACITY)
            } else {
                link
            };
            link.with_match_deserializer(|_ctx, m, bytes| {
                Ok(reading(
                    bytes[0] as usize + m.key().map_or(0, |k| k.index()),
                ))
            })
            .finish();
        });
        add_decoys(b);
    })
    .await
}

/// `n` copies of one topic, or `n` topics each naming a new device.
fn pattern_topics(n: usize, first_device: usize, distinct: bool) -> Vec<String> {
    (0..n)
        .map(|i| {
            let device = if distinct { first_device + i } else { 0 };
            format!("in/dev{device}/target")
        })
        .collect()
}

/// One exact topic among the decoys.
async fn measure_dispatch() -> (u64, u64) {
    let db = inbound_db().await;
    let inbound = InboundDispatch::new(&db, SCHEME, &ExactGrammar).unwrap();
    let payload = [1u8; 8];
    for _ in 0..WARMUP_ITERS {
        inbound.dispatch("in/target", &payload);
    }
    reset();
    for _ in 0..MEASURE_ITERS {
        inbound.dispatch("in/target", black_box(&payload));
    }
    snapshot()
}

/// Dispatches `warmup` then `measured`, counting only the second.
async fn measure_pattern_dispatch(
    keyed: bool,
    warmup: &[String],
    measured: &[String],
) -> (u64, u64) {
    let db = pattern_db(keyed).await;
    let inbound = InboundDispatch::new(&db, SCHEME, &MqttGrammar).unwrap();
    let payload = [1u8; 8];
    for topic in warmup {
        inbound.dispatch(topic, &payload);
    }
    reset();
    for topic in measured {
        inbound.dispatch(black_box(topic), black_box(&payload));
    }
    snapshot()
}

// --- Outbound ---------------------------------------------------------------

#[derive(Clone, Copy)]
enum Serializer {
    Scratch,
    Owned,
}

#[derive(Clone, Copy)]
enum Topic {
    Static,
    Written,
}

/// One outbound link on `record` with the given serializer and topic.
fn outbound_link(
    reg: &mut aimdb_core::RecordRegistrar<'_, Reading>,
    serializer: Serializer,
    topic: Topic,
) {
    reg.buffer(BufferCfg::SpmcRing { capacity: 64 });
    let mut link = reg
        .link_to("bench://out/default")
        .with_serializer(|_ctx, r: &Reading| Ok(encode(r).to_vec()));
    if let Serializer::Scratch = serializer {
        link = link.with_serializer_into(SCRATCH_CAPACITY, |_ctx, r: &Reading, buf| {
            let bytes = encode(r);
            let dst = buf
                .get_mut(..bytes.len())
                .ok_or(SerializeError::BufferTooSmall)?;
            dst.copy_from_slice(&bytes);
            Ok(bytes.len())
        });
    }
    if let Topic::Written = topic {
        link = link.with_topic_fn(16, |r, out| {
            use std::fmt::Write as _;
            write!(out, "out/{}", r.id)?;
            Ok(true)
        });
    }
    link.finish();
}

/// Pulls one message and hands its topic and bytes to `black_box`.
async fn pull_one(outbound: &mut OutboundRoutes) {
    let msg = outbound.next().await.expect("route open");
    black_box((msg.topic, msg.payload.as_slice()));
}

async fn measure_next(serializer: Serializer, topic: Topic) -> (u64, u64) {
    let db = build_db(|b| {
        b.configure::<Reading>("out.record", move |reg| {
            outbound_link(reg, serializer, topic)
        });
    })
    .await;
    let mut outbound = OutboundRoutes::new(&db, SCHEME).unwrap();
    let producer = db.producer::<Reading>("out.record").expect("producer");

    for i in 0..WARMUP_ITERS {
        producer.produce(reading(i));
        pull_one(&mut outbound).await;
    }
    reset();
    for i in 0..MEASURE_ITERS {
        producer.produce(reading(i));
        pull_one(&mut outbound).await;
    }
    snapshot()
}

/// One value on each of `ROUND_ROBIN_ROUTES` routes, then as many pulls.
async fn measure_next_round_robin() -> (u64, u64) {
    let db = build_db(|b| {
        for i in 0..ROUND_ROBIN_ROUTES {
            b.configure::<Reading>(StringKey::intern(format!("out.rr{i}")), |reg| {
                outbound_link(reg, Serializer::Scratch, Topic::Static)
            });
        }
    })
    .await;
    let mut outbound = OutboundRoutes::new(&db, SCHEME).unwrap();
    let producers: Vec<_> = (0..ROUND_ROBIN_ROUTES)
        .map(|i| {
            db.producer::<Reading>(format!("out.rr{i}"))
                .expect("producer")
        })
        .collect();
    for i in 0..WARMUP_ITERS / ROUND_ROBIN_ROUTES {
        round_robin(&mut outbound, &producers, i).await;
    }
    reset();
    for i in 0..MEASURE_ITERS / ROUND_ROBIN_ROUTES {
        round_robin(&mut outbound, &producers, i).await;
    }
    snapshot()
}

async fn round_robin(
    outbound: &mut OutboundRoutes,
    producers: &[aimdb_core::Producer<Reading>],
    i: usize,
) {
    for p in producers {
        p.produce(reading(i));
    }
    for _ in 0..producers.len() {
        pull_one(outbound).await;
    }
}

/// The transport pulls in its own task and parks before every value; the
/// producer writes one value, then yields until it was pulled.
async fn measure_next_parked() -> (u64, u64) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let db = build_db(|b| {
        b.configure::<Reading>("out.record", |reg| {
            outbound_link(reg, Serializer::Scratch, Topic::Static)
        });
    })
    .await;
    let mut outbound = OutboundRoutes::new(&db, SCHEME).unwrap();
    let producer = db.producer::<Reading>("out.record").expect("producer");
    let pulled = Arc::new(AtomicUsize::new(0));
    let total = WARMUP_ITERS + MEASURE_ITERS;
    let transport = {
        let pulled = pulled.clone();
        tokio::spawn(async move {
            for _ in 0..total {
                pull_one(&mut outbound).await;
                pulled.fetch_add(1, Ordering::Release);
            }
        })
    };
    let send = |i: usize| {
        producer.produce(reading(i));
        let pulled = pulled.clone();
        async move {
            while pulled.load(Ordering::Acquire) <= i {
                tokio::task::yield_now().await;
            }
        }
    };

    for i in 0..WARMUP_ITERS {
        send(i).await;
    }
    reset();
    for i in WARMUP_ITERS..total {
        send(i).await;
    }
    let counted = snapshot();
    transport.await.expect("transport");
    counted
}

// --- Driver -----------------------------------------------------------------

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("bench runtime");

    let measured: Vec<(&str, &str, (u64, u64))> = runtime.block_on(async {
        vec![
            ("inbound_dispatch", "SpmcRing", measure_dispatch().await),
            (
                "inbound_dispatch_pattern",
                "SpmcRing",
                measure_pattern_dispatch(
                    false,
                    &pattern_topics(WARMUP_ITERS, 0, false),
                    &pattern_topics(MEASURE_ITERS, 0, false),
                )
                .await,
            ),
            (
                "inbound_dispatch_keyed_known",
                "SpmcRing",
                measure_pattern_dispatch(
                    true,
                    &pattern_topics(WARMUP_ITERS, 0, false),
                    &pattern_topics(MEASURE_ITERS, 0, false),
                )
                .await,
            ),
            (
                "inbound_dispatch_keyed_new",
                "SpmcRing",
                measure_pattern_dispatch(
                    true,
                    &pattern_topics(WARMUP_ITERS, 0, true),
                    &pattern_topics(MEASURE_ITERS, WARMUP_ITERS, true),
                )
                .await,
            ),
            (
                "outbound_next_static_topic",
                "SpmcRing",
                measure_next(Serializer::Scratch, Topic::Static).await,
            ),
            (
                "outbound_next_written_topic",
                "SpmcRing",
                measure_next(Serializer::Scratch, Topic::Written).await,
            ),
            (
                "outbound_next_owned",
                "SpmcRing",
                measure_next(Serializer::Owned, Topic::Static).await,
            ),
            (
                "outbound_next_round_robin",
                "SpmcRing",
                measure_next_round_robin().await,
            ),
            (
                "outbound_next_parked",
                "SpmcRing",
                measure_next_parked().await,
            ),
        ]
    });

    println!("=== B0 connector boundary: allocations per message ===");
    let mut reports = Vec::with_capacity(measured.len());
    let mut mismatches = Vec::new();
    for &(case, buffer, (allocs, bytes)) in &measured {
        let report = AllocReport::new(case, buffer, MEASURE_ITERS, allocs, bytes);
        println!(
            "{case:<34} {:>5.2} allocs/msg {:>7.1} bytes/msg",
            report.allocs_per_msg, report.bytes_per_msg
        );
        let expected = EXPECTED
            .iter()
            .find(|(name, _)| *name == case)
            .map(|(_, n)| *n)
            .expect("every case has an expected value");
        if report.allocs_per_msg.round() as u64 != expected {
            mismatches.push(format!(
                "{case}: expected {expected}, measured {:.2}",
                report.allocs_per_msg
            ));
        }
        reports.push(report);
    }
    write_reports("b0_alloc_connector", &reports);

    assert!(
        mismatches.is_empty(),
        "connector-boundary allocations changed — update EXPECTED and the baseline:\n  {}",
        mismatches.join("\n  ")
    );
}
