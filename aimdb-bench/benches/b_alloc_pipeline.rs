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
//! **Pacing (design 039 F15).** Batch start/completion use `tokio::sync::watch`
//! (permit-carrying: `changed()`/`borrow_and_update()` cannot miss a value the
//! way `Notify::notified()` can when `notify_waiters()` fires before the
//! waiter starts listening — same primitive the F3 fix uses for the same
//! reason). Per-message pacing uses `tokio::sync::Semaphore`, the standard
//! counting-permit primitive, in place of a hand-rolled atomic token count +
//! `Notify`. Both are lost-wakeup-free on any executor — no current-thread
//! requirement, unlike the check-then-await pattern this replaces.

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
    reports::{write_reports, AllocReport},
};
use aimdb_core::{buffer::BufferCfg, AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use tokio::sync::{oneshot, watch, Semaphore};

// One `#[global_allocator]` per bench binary (design 039 F12).
#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

struct BatchState {
    /// Published by `run_batch` when a new batch starts; the source task
    /// awaits changes on its own receiver.
    start_epoch_tx: watch::Sender<u64>,
    /// Published by the tap when it finishes consuming a batch;
    /// `run_batch` awaits changes on its own receiver.
    completed_epoch_tx: watch::Sender<u64>,
    batch_size: AtomicUsize,
    target_epoch: AtomicU64,
    consumed_in_epoch: AtomicUsize,
    /// One permit per message the source is allowed to produce without
    /// waiting for the tap to catch up — `run_batch` grants the first,
    /// the tap grants each subsequent one after consuming a message.
    pace: Semaphore,
}

impl BatchState {
    fn new() -> Self {
        let (start_epoch_tx, _) = watch::channel(0u64);
        let (completed_epoch_tx, _) = watch::channel(0u64);
        Self {
            start_epoch_tx,
            completed_epoch_tx,
            batch_size: AtomicUsize::new(0),
            target_epoch: AtomicU64::new(0),
            consumed_in_epoch: AtomicUsize::new(0),
            pace: Semaphore::new(0),
        }
    }
}

struct PipelineHarness {
    _db: AimDb,
    state: Arc<BatchState>,
    completed_epoch_rx: watch::Receiver<u64>,
}

impl PipelineHarness {
    async fn warmup(&mut self, batch_size: usize) {
        self.run_batch(batch_size).await;
    }

    async fn measure(&mut self, batch_size: usize) {
        self.run_batch(batch_size).await;
    }

    async fn run_batch(&mut self, batch_size: usize) {
        let epoch = *self.state.start_epoch_tx.borrow() + 1;
        self.state.batch_size.store(batch_size, Ordering::Release);
        self.state.consumed_in_epoch.store(0, Ordering::Release);
        self.state.target_epoch.store(epoch, Ordering::Release);
        self.state.pace.add_permits(1);
        let _ = self.state.start_epoch_tx.send(epoch);

        while *self.completed_epoch_rx.borrow_and_update() < epoch {
            self.completed_epoch_rx
                .changed()
                .await
                .expect("pipeline tap task exited");
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
    let completed_epoch_rx = state.completed_epoch_tx.subscribe();
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
            let mut start_epoch_rx = source_state.start_epoch_tx.subscribe();

            reg.buffer(cfg)
                .source(move |_ctx, producer| async move {
                    let mut next_value_index = 0u64;
                    let mut seen_epoch = 0u64;
                    loop {
                        while *start_epoch_rx.borrow_and_update() == seen_epoch {
                            start_epoch_rx
                                .changed()
                                .await
                                .expect("pipeline harness dropped");
                        }
                        seen_epoch = *start_epoch_rx.borrow();

                        let batch_size = source_state.batch_size.load(Ordering::Acquire);
                        for _ in 0..batch_size {
                            source_state
                                .pace
                                .acquire()
                                .await
                                .expect("pace semaphore closed")
                                .forget();
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
                            tap_state.pace.add_permits(1);
                        }
                        if seen == batch_size {
                            let _ = tap_state.completed_epoch_tx.send(current_epoch);
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

    PipelineHarness {
        _db: db,
        state,
        completed_epoch_rx,
    }
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
        let mut harness = build_pipeline_harness::<TelemetryMsg, _>(
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
        let mut harness = build_pipeline_harness::<StateMsg, _>(
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
        let mut harness = build_pipeline_harness::<CommandMsg, _>(
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

    write_reports(
        "b_alloc_pipeline",
        &[telemetry_report, state_report, command_report],
    );
}
