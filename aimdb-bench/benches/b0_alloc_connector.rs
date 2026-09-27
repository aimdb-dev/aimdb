//! B0-Connector — per-message allocations at the connector boundary.
//!
//! Baseline for design 054. Measures what AimDB's connector interfaces cost
//! per message, independent of any real transport:
//!
//! - **Inbound:** `Router::route` alone, and the real `pump_source` driven by
//!   the smallest possible `Source` (it clones a pre-built topic `String` and
//!   payload `Arc` — the least any `Source` can do, since the trait returns
//!   owned values).
//! - **Outbound:** the per-message calls `pump_sink` makes —
//!   `SerializedReader::recv_into` followed by `Connector::publish` on a no-op
//!   connector — for the scratch and owned serializers, with a static and a
//!   dynamic (`TopicProvider`) topic.
//!
//! Buffers, ingest and routing allocate nothing (design 037, and the `route`
//! row here); every non-zero row is a cost of the connector interface. The
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
use aimdb_core::connector::{
    ConnectorBuilder, SerializeError, SerializedPayload, SerializedReader, SerializedValueInto,
    TopicProvider,
};
use aimdb_core::router::RouterBuilder;
use aimdb_core::session::{pump_source, Payload, Source};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{AimDb, AimDbBuilder, BoxFut, DbResult, RuntimeContext, StringKey};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

const WARMUP_ITERS: usize = 100;
const MEASURE_ITERS: usize = 2_000;
const DECOY_ROUTES: usize = 63;
const SCRATCH_CAPACITY: usize = 64;

/// Allocations per message on `main` when this bench was added. Update
/// together with `data/baselines/b0_alloc_connector.json`.
const EXPECTED: &[(&str, u64)] = &[
    ("inbound_route", 0),
    ("inbound_pump_source_minimal", 2),
    ("outbound_scratch_static_topic", 2),
    ("outbound_scratch_dynamic_topic", 3),
    ("outbound_owned_static_topic", 3),
];

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

/// Returns a ready future, boxed as the `Connector` trait requires.
struct NoopSink;

impl Connector for NoopSink {
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        black_box((destination, payload));
        Box::pin(async { Ok(()) })
    }
}

struct IdTopic;

impl TopicProvider<Reading> for IdTopic {
    fn topic(&self, value: &Reading) -> Option<String> {
        Some(format!("out/{}", value.id))
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

/// One linked record plus `DECOY_ROUTES` others, so routing scans a realistic
/// table.
async fn inbound_db() -> AimDb {
    build_db(|b| {
        b.configure::<Reading>("in.target", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
                .link_from("bench://in/target")
                .with_deserializer(|_ctx, bytes| Ok(reading(bytes[0] as usize)))
                .finish();
        });
        for i in 0..DECOY_ROUTES {
            let topic = format!("bench://in/decoy/{i}");
            b.configure::<Reading>(StringKey::intern(format!("in.decoy{i}")), |reg| {
                reg.buffer(BufferCfg::SingleLatest)
                    .link_from(&topic)
                    .with_deserializer(|_ctx, _bytes| Ok(reading(0)))
                    .finish();
            });
        }
    })
    .await
}

async fn measure_route() -> (u64, u64) {
    let db = inbound_db().await;
    let ctx = db.runtime_ctx();
    let router = RouterBuilder::from_routes(db.collect_inbound_routes(SCHEME)).build();
    let payload = [1u8; 8];
    for _ in 0..WARMUP_ITERS {
        router.route("in/target", &payload, &ctx).unwrap();
    }
    reset();
    for _ in 0..MEASURE_ITERS {
        router
            .route("in/target", black_box(&payload), &ctx)
            .unwrap();
    }
    snapshot()
}

/// Yields `remaining` copies of one message, then ends.
struct MinimalSource {
    topic: String,
    payload: Payload,
    remaining: usize,
}

impl Source for MinimalSource {
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move {
            if self.remaining == 0 {
                return None;
            }
            self.remaining -= 1;
            Some((self.topic.clone(), self.payload.clone()))
        })
    }
}

/// Allocations of one complete `pump_source` run over `messages` messages,
/// including its one-off setup.
async fn pump_run(db: &AimDb, messages: usize) -> (u64, u64) {
    let source = MinimalSource {
        topic: "in/target".to_string(),
        payload: Arc::from(&[1u8; 8][..]),
        remaining: messages,
    };
    reset();
    for fut in pump_source(db, SCHEME, source) {
        fut.await;
    }
    snapshot()
}

/// Per-message cost as the difference of two runs, so pump setup (router
/// build, start-up logging) cancels out.
async fn measure_pump_source() -> (u64, u64) {
    let db = inbound_db().await;
    pump_run(&db, WARMUP_ITERS).await;
    let short = pump_run(&db, WARMUP_ITERS).await;
    let long = pump_run(&db, WARMUP_ITERS + MEASURE_ITERS).await;
    (long.0 - short.0, long.1 - short.1)
}

// --- Outbound ---------------------------------------------------------------

/// The per-route state `pump_sink` keeps, and one iteration of its loop.
struct OutboundIo {
    reader: Box<dyn SerializedReader>,
    scratch: Vec<u8>,
    default_topic: String,
    config: ConnectorConfig,
}

impl OutboundIo {
    async fn publish_one(&mut self, ctx: &RuntimeContext) {
        let SerializedValueInto { dest, payload } = self
            .reader
            .recv_into(ctx, &mut self.scratch)
            .await
            .expect("recv_into");
        let dest = dest.as_deref().unwrap_or(&self.default_topic);
        let bytes: &[u8] = match &payload {
            SerializedPayload::Scratch { len } => &self.scratch[..*len],
            SerializedPayload::Owned(v) => v,
        };
        NoopSink
            .publish(dest, &self.config, bytes)
            .await
            .expect("publish");
    }
}

#[derive(Clone, Copy)]
enum Serializer {
    Scratch,
    Owned,
}

async fn measure_outbound(serializer: Serializer, dynamic_topic: bool) -> (u64, u64) {
    let db = build_db(|b| {
        b.configure::<Reading>("out.record", |reg| {
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
            if dynamic_topic {
                link = link.with_topic_provider(IdTopic);
            }
            link.finish();
        });
    })
    .await;

    let ctx = db.runtime_ctx();
    let route = db
        .collect_outbound_routes(SCHEME)
        .pop()
        .expect("one outbound route");
    let mut io = OutboundIo {
        reader: route.source.subscribe(),
        scratch: vec![0u8; route.source.serializer_scratch_capacity().unwrap_or(0)],
        default_topic: route.topic.clone(),
        config: ConnectorConfig::from_query(&route.config),
    };
    let producer = db.producer::<Reading>("out.record").expect("producer");

    for i in 0..WARMUP_ITERS {
        producer.produce(reading(i));
        io.publish_one(&ctx).await;
    }
    reset();
    for i in 0..MEASURE_ITERS {
        producer.produce(reading(i));
        io.publish_one(&ctx).await;
    }
    snapshot()
}

// --- Driver -----------------------------------------------------------------

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("bench runtime");

    let measured: Vec<(&str, &str, (u64, u64))> = runtime.block_on(async {
        vec![
            ("inbound_route", "SpmcRing", measure_route().await),
            (
                "inbound_pump_source_minimal",
                "SpmcRing",
                measure_pump_source().await,
            ),
            (
                "outbound_scratch_static_topic",
                "SpmcRing",
                measure_outbound(Serializer::Scratch, false).await,
            ),
            (
                "outbound_scratch_dynamic_topic",
                "SpmcRing",
                measure_outbound(Serializer::Scratch, true).await,
            ),
            (
                "outbound_owned_static_topic",
                "SpmcRing",
                measure_outbound(Serializer::Owned, false).await,
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
