//! B-Runner-Pipeline — Runner-driven in-process pipeline throughput.
//!
//! Exercises the same three profiles as B0/B1/B2 through a real `AimDbRunner`
//! path: `.source()` -> buffer -> `.tap()`.  This makes the benchmark measure
//! stage wakeups and the runner-driven producer/consumer pipeline rather than
//! direct `Producer<T>` / `Consumer<T>` calls from the bench body.
//!
//! **Scope:** this is a real runner benchmark, but still in-process only.
//! It does not include outbound connectors, serialization, transport, or
//! kernel I/O. The timing window includes the coordination handshakes used to
//! feed messages into the source stage and observe completion at the tap stage.
//!
//! **Setup:** one `AimDb` instance is built per bench group, and its runner is
//! spawned once. Criterion samples then push work into the source stage via an
//! ingress channel and wait for completion signals emitted by the tap stage.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b_runner_pipeline
//! ```

use std::fmt::Debug;
use std::sync::Arc;

use aimdb_bench::profiles::{
    command_msg, state_msg, telemetry_msg, CommandMsg, StateMsg, TelemetryMsg, TELEMETRY_CAPACITY,
    WARMUP_ITERS,
};
use aimdb_core::{buffer::BufferCfg, AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot,
};

struct RunnerHarness<T> {
    _db: AimDb,
    input_tx: UnboundedSender<T>,
    ack_rx: UnboundedReceiver<()>,
}

impl<T> RunnerHarness<T> {
    async fn round_trip(&mut self, value: T) {
        self.input_tx
            .send(value)
            .expect("source input channel closed unexpectedly");
        self.ack_rx
            .recv()
            .await
            .expect("tap acknowledgement channel closed unexpectedly");
    }
}

async fn build_runner_harness<T>(key: &'static str, cfg: BufferCfg) -> RunnerHarness<T>
where
    T: Send + Sync + Clone + Debug + 'static,
{
    let (input_tx, mut input_rx) = unbounded_channel::<T>();
    let (ack_tx, ack_rx) = unbounded_channel::<()>();
    let (tap_ready_tx, tap_ready_rx) = oneshot::channel::<()>();

    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    builder.configure::<T>(key, move |reg| {
        let ack_tx = ack_tx.clone();
        let mut tap_ready_tx = Some(tap_ready_tx);

        reg.buffer(cfg)
            .source(move |_ctx, producer| async move {
                while let Some(value) = input_rx.recv().await {
                    producer.produce(value);
                }
            })
            .with_name("bench_source")
            .tap(move |_ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                tap_ready_tx
                    .take()
                    .expect("tap readiness sender already used")
                    .send(())
                    .expect("failed to signal tap readiness");
                while let Ok(_value) = reader.recv().await {
                    ack_tx
                        .send(())
                        .expect("bench tap ack channel closed unexpectedly");
                }
            })
            .with_name("bench_tap");
    });

    let (db, runner) = builder.build().await.expect("runner bench build failed");
    tokio::spawn(runner.run());
    tap_ready_rx
        .await
        .expect("runner tap exited before signalling readiness");

    RunnerHarness {
        _db: db,
        input_tx,
        ack_rx,
    }
}

// ── Telemetry E2E ─────────────────────────────────────────────────────────────

fn bench_e2e_telemetry(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut harness = rt.block_on(build_runner_harness::<TelemetryMsg>(
        "bench::telemetry",
        BufferCfg::SpmcRing {
            capacity: TELEMETRY_CAPACITY,
        },
    ));

    let mut group = c.benchmark_group("B-Runner-Pipeline");
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                for i in 0..WARMUP_ITERS {
                    harness.round_trip(telemetry_msg(i as u64)).await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    harness
                        .round_trip(telemetry_msg((WARMUP_ITERS as u64) + i))
                        .await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── State E2E ─────────────────────────────────────────────────────────────────

fn bench_e2e_state(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut harness = rt.block_on(build_runner_harness::<StateMsg>(
        "bench::state",
        BufferCfg::SingleLatest,
    ));

    let mut group = c.benchmark_group("B-Runner-Pipeline");
    group.throughput(Throughput::Elements(1));

    group.bench_function("state_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                for i in 0..WARMUP_ITERS {
                    harness.round_trip(state_msg(i as u64)).await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    harness
                        .round_trip(state_msg((WARMUP_ITERS as u64) + i))
                        .await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── Command E2E ───────────────────────────────────────────────────────────────

fn bench_e2e_command(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut harness = rt.block_on(build_runner_harness::<CommandMsg>(
        "bench::command",
        BufferCfg::Mailbox,
    ));

    let mut group = c.benchmark_group("B-Runner-Pipeline");
    group.throughput(Throughput::Elements(1));

    group.bench_function("command_mailbox", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                for i in 0..WARMUP_ITERS {
                    harness.round_trip(command_msg(i as u64)).await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    harness
                        .round_trip(command_msg((WARMUP_ITERS as u64) + i))
                        .await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_e2e_telemetry,
    bench_e2e_state,
    bench_e2e_command,
);
criterion_main!(benches);
