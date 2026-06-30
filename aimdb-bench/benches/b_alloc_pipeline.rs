//! B0-Pipeline — Allocation counting for a live runner-driven pipeline.
//!
//! Measures per-message allocation cost for a real `.source()` -> buffer ->
//! `.tap()` pipeline driven by `AimDbRunner` — an integration-layer measurement
//! that includes runner/stage machinery on top of the buffer consume path. The
//! source generates each batch internally after a single start notification and
//! the tap signals completion once the batch is consumed, so the measured window
//! carries no per-message ingress or ack traffic.
//!
//! Treat this as an informational companion to the raw-buffer B0 gate in
//! `b0_alloc_tokio`: if it regresses, that gate still isolates whether the
//! consume path itself is at fault.
//!
//! Run `cargo bench -p aimdb-bench --bench b_alloc_pipeline`; results are written
//! to `aimdb-bench/target/bench-results/b_alloc_pipeline.json` (anchored to the
//! crate dir).
//!
//! **Executor dependency.** The source/tap pacing uses check-then-await (load an
//! atomic, `.notified().await` only when there is no work). `notify_waiters()`
//! stores no permit, so this avoids lost wakeups only because the bench runs on
//! a **current-thread** runtime: nothing preempts between the load and the
//! `.await`. Do not port to a multi-threaded executor without revisiting it.

use std::fmt::Debug;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use aimdb_bench::{
    alloc::{reset, snapshot},
    profiles::{
        command_msg, state_msg, telemetry_msg, CommandMsg, StateMsg, TelemetryMsg, BATCH_SIZE,
        TELEMETRY_CAPACITY, WARMUP_ITERS,
    },
    reports::AllocReport,
};
use aimdb_core::{buffer::BufferCfg, AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use tokio::sync::{oneshot, Notify};

struct BatchState {
    start_epoch: AtomicU64,
    completed_epoch: AtomicU64,
    batch_size: AtomicUsize,
    target_epoch: AtomicU64,
    consumed_in_epoch: AtomicUsize,
    start_notify: Notify,
    pace_tokens: AtomicUsize,
    pace_notify: Notify,
    done_notify: Notify,
}

impl BatchState {
    fn new() -> Self {
        Self {
            start_epoch: AtomicU64::new(0),
            completed_epoch: AtomicU64::new(0),
            batch_size: AtomicUsize::new(0),
            target_epoch: AtomicU64::new(0),
            consumed_in_epoch: AtomicUsize::new(0),
            start_notify: Notify::new(),
            pace_tokens: AtomicUsize::new(0),
            pace_notify: Notify::new(),
            done_notify: Notify::new(),
        }
    }
}

struct PipelineHarness {
    _db: AimDb,
    state: Arc<BatchState>,
}

impl PipelineHarness {
    async fn warmup(&self, batch_size: usize) {
        self.run_batch(batch_size).await;
    }

    async fn measure(&self, batch_size: usize) {
        self.run_batch(batch_size).await;
    }

    async fn run_batch(&self, batch_size: usize) {
        let epoch = self.state.start_epoch.load(Ordering::Acquire) + 1;
        self.state.batch_size.store(batch_size, Ordering::Release);
        self.state.consumed_in_epoch.store(0, Ordering::Release);
        self.state.target_epoch.store(epoch, Ordering::Release);
        self.state.pace_tokens.store(1, Ordering::Release);
        self.state.start_epoch.store(epoch, Ordering::Release);
        self.state.start_notify.notify_waiters();
        self.state.pace_notify.notify_waiters();

        while self.state.completed_epoch.load(Ordering::Acquire) < epoch {
            self.state.done_notify.notified().await;
        }
    }
}

async fn build_pipeline_harness<T, Make>(
    key: &'static str,
    cfg: BufferCfg,
    make_value: Make,
) -> PipelineHarness
where
    T: Send + Sync + Clone + Debug + 'static,
    Make: Fn(u64) -> T + Send + Sync + Clone + 'static,
{
    let state = Arc::new(BatchState::new());
    let (tap_ready_tx, tap_ready_rx) = oneshot::channel::<()>();

    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);
    builder.configure::<T>(key, {
        let state = Arc::clone(&state);
        move |reg| {
            let source_state = Arc::clone(&state);
            let tap_state = Arc::clone(&state);
            let mut tap_ready_tx = Some(tap_ready_tx);
            let make_value = make_value.clone();

            reg.buffer(cfg)
                .source(move |_ctx, producer| async move {
                    let mut next_value_index = 0u64;
                    let mut seen_epoch = 0u64;
                    loop {
                        while source_state.start_epoch.load(Ordering::Acquire) == seen_epoch {
                            source_state.start_notify.notified().await;
                        }

                        seen_epoch = source_state.start_epoch.load(Ordering::Acquire);
                        let batch_size = source_state.batch_size.load(Ordering::Acquire);
                        for _ in 0..batch_size {
                            loop {
                                let available = source_state.pace_tokens.load(Ordering::Acquire);
                                if available > 0 {
                                    if source_state
                                        .pace_tokens
                                        .compare_exchange(
                                            available,
                                            available - 1,
                                            Ordering::AcqRel,
                                            Ordering::Acquire,
                                        )
                                        .is_ok()
                                    {
                                        break;
                                    }
                                } else {
                                    source_state.pace_notify.notified().await;
                                }
                            }
                            producer.produce(make_value(next_value_index));
                            next_value_index += 1;
                        }
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
                        let current_epoch = tap_state.target_epoch.load(Ordering::Acquire);
                        let seen = tap_state.consumed_in_epoch.fetch_add(1, Ordering::AcqRel) + 1;
                        let batch_size = tap_state.batch_size.load(Ordering::Acquire);
                        if seen < batch_size {
                            tap_state.pace_tokens.fetch_add(1, Ordering::AcqRel);
                            tap_state.pace_notify.notify_waiters();
                        }
                        if seen == batch_size {
                            tap_state
                                .completed_epoch
                                .store(current_epoch, Ordering::Release);
                            tap_state.done_notify.notify_waiters();
                        }
                    }
                })
                .with_name("bench_tap");
        }
    });

    let (db, runner) = builder
        .build()
        .await
        .expect("alloc pipeline bench build failed");
    tokio::spawn(runner.run());
    tap_ready_rx
        .await
        .expect("pipeline tap exited before signalling readiness");

    PipelineHarness { _db: db, state }
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("failed to build current-thread Tokio runtime");

    println!("=== B0 Allocation Benchmarks (Runner pipeline) ===");
    println!("  Warmup batch : {WARMUP_ITERS}");
    println!("  Measured batch: {BATCH_SIZE}");
    println!();

    let telemetry_report = rt.block_on(async {
        let harness = build_pipeline_harness::<TelemetryMsg, _>(
            "bench::telemetry",
            BufferCfg::SpmcRing {
                capacity: TELEMETRY_CAPACITY,
            },
            telemetry_msg,
        )
        .await;

        harness.warmup(WARMUP_ITERS).await;
        reset();
        harness.measure(BATCH_SIZE).await;
        let (allocs, bytes) = snapshot();
        AllocReport::new("Telemetry", "SpmcRing", BATCH_SIZE, allocs, bytes)
    });
    telemetry_report.print();

    let state_report = rt.block_on(async {
        let harness = build_pipeline_harness::<StateMsg, _>(
            "bench::state",
            BufferCfg::SingleLatest,
            state_msg,
        )
        .await;

        harness.warmup(WARMUP_ITERS).await;
        reset();
        harness.measure(BATCH_SIZE).await;
        let (allocs, bytes) = snapshot();
        AllocReport::new("State", "SingleLatest", BATCH_SIZE, allocs, bytes)
    });
    state_report.print();

    let command_report = rt.block_on(async {
        let harness = build_pipeline_harness::<CommandMsg, _>(
            "bench::command",
            BufferCfg::Mailbox,
            command_msg,
        )
        .await;

        harness.warmup(WARMUP_ITERS).await;
        reset();
        harness.measure(BATCH_SIZE).await;
        let (allocs, bytes) = snapshot();
        AllocReport::new("Command", "Mailbox", BATCH_SIZE, allocs, bytes)
    });
    command_report.print();

    let reports = vec![telemetry_report, state_report, command_report];
    let json = serde_json::to_string_pretty(&reports).expect("failed to serialize reports");
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/bench-results");
    std::fs::create_dir_all(out_dir).expect("failed to create results directory");
    let out_path = format!("{out_dir}/b_alloc_pipeline.json");
    std::fs::write(&out_path, &json).expect("failed to write results");
    println!("\nResults written to {out_path}");
}
