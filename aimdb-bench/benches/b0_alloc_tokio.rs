//! B0 — Allocation counting on the Tokio adapter.
//!
//! Measures per-message allocation cost for each workload profile using
//! `TokioBuffer<T>` directly (not the full `AimDb` stack).
//!
//! **Pre-W8 baseline (expected):** 1 alloc/msg from the `Box::pin(async
//! move { ... })` constructed inside `TokioBufferReader::recv()` on every
//! call.  The target is **0 allocs/msg**.
//!
//! **Measurement model:**
//! 1. Create buffer + reader.
//! 2. Warmup ≥ `WARMUP_ITERS` push → recv cycles (excluded from counters).
//! 3. `reset()` allocation counters.
//! 4. Run `BATCH_SIZE` push → recv cycles.
//! 5. `snapshot()` counters; divide by `BATCH_SIZE` for per-message figures.
//!
//! **Noise reduction:** a current-thread Tokio runtime is used so there are
//! no work-stealing threads and Tokio's scheduler does not allocate per-poll
//! in the hot path.  The counter then cleanly isolates AimDB's per-message
//! contribution.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b0_alloc_tokio
//! ```
//!
//! Results are written to `target/bench-results/b0_alloc_tokio.json`.

use aimdb_bench::{
    alloc::{reset, snapshot},
    profiles::{
        command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
        BATCH_SIZE, WARMUP_ITERS,
    },
    reports::AllocReport,
};
use aimdb_core::buffer::{Buffer, BufferReader};

fn main() {
    // Current-thread executor — no work-stealing threads, minimal scheduler
    // overhead, clean allocation signal.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("failed to build current-thread Tokio runtime");

    println!("=== B0 Allocation Benchmarks (Tokio adapter, buffer layer) ===");
    println!("  Warmup iters : {WARMUP_ITERS}");
    println!("  Batch size   : {BATCH_SIZE}");
    println!();

    // ── Telemetry: SpmcRing / broadcast ─────────────────────────────────────
    //
    // Subscribe before pushing so the reader holds its read position from the
    // start — a reader created after sends are in flight misses them.
    let telemetry_report = rt.block_on(async {
        let buf = telemetry_buffer();
        let mut reader = buf.subscribe();

        for i in 0..WARMUP_ITERS {
            buf.push(telemetry_msg(i as u64));
            let _ = reader.recv().await;
        }

        reset();
        for i in 0..BATCH_SIZE {
            buf.push(telemetry_msg((WARMUP_ITERS + i) as u64));
            let _ = reader.recv().await;
        }
        let (allocs, bytes) = snapshot();
        AllocReport::new("Telemetry", "SpmcRing", BATCH_SIZE, allocs, bytes)
    });
    telemetry_report.print();

    // ── State: SingleLatest / watch ──────────────────────────────────────────
    let state_report = rt.block_on(async {
        let buf = state_buffer();
        let mut reader = buf.subscribe();

        for i in 0..WARMUP_ITERS {
            buf.push(state_msg(i as u64));
            let _ = reader.recv().await;
        }

        reset();
        for i in 0..BATCH_SIZE {
            buf.push(state_msg((WARMUP_ITERS + i) as u64));
            let _ = reader.recv().await;
        }
        let (allocs, bytes) = snapshot();
        AllocReport::new("State", "SingleLatest", BATCH_SIZE, allocs, bytes)
    });
    state_report.print();

    // ── Command: Mailbox / Mutex + Notify ────────────────────────────────────
    //
    // Tight 1:1 push → recv loop matches Mailbox semantics.  Do NOT batch
    // pushes ahead of the consumer: the single slot overwrites earlier values,
    // and only the last write survives — which would conflate Mailbox overwrite
    // semantics with throughput measurement.
    let command_report = rt.block_on(async {
        let buf = command_buffer();
        let mut reader = buf.subscribe();

        for i in 0..WARMUP_ITERS {
            buf.push(command_msg(i as u64));
            let _ = reader.recv().await;
        }

        reset();
        for i in 0..BATCH_SIZE {
            buf.push(command_msg((WARMUP_ITERS + i) as u64));
            let _ = reader.recv().await;
        }
        let (allocs, bytes) = snapshot();
        AllocReport::new("Command", "Mailbox", BATCH_SIZE, allocs, bytes)
    });
    command_report.print();

    println!();
    println!(
        "Pre-W8 expectation: ~1 alloc/msg (Box::pin in recv()). \
         Target: 0 allocs/msg."
    );

    // Persist results for baseline comparison.
    let reports = vec![telemetry_report, state_report, command_report];
    let json = serde_json::to_string_pretty(&reports).expect("failed to serialize reports");
    let out_dir = "target/bench-results";
    std::fs::create_dir_all(out_dir).ok();
    let out_path = format!("{out_dir}/b0_alloc_tokio.json");
    std::fs::write(&out_path, &json).expect("failed to write results");
    println!("\nResults written to {out_path}");
}
