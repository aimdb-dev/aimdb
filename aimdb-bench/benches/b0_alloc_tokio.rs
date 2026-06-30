//! B0 — Allocation counting on the Tokio adapter.
//!
//! Measures per-message allocation cost for each workload profile against
//! `TokioBuffer<T>` directly (not the full `AimDb` stack). The consume path
//! polls the reader without boxing a future per `recv()`, so the expected
//! result is **0 allocs/msg**; a non-zero figure flags an allocation regression
//! in the consume path.
//!
//! **Measurement model:** create buffer + reader, warm up `WARMUP_ITERS`
//! push → recv cycles, `reset()` the counters, run `BATCH_SIZE` cycles, then
//! `snapshot()` and divide by `BATCH_SIZE`. A current-thread Tokio runtime
//! keeps scheduler allocation out of the hot path so the counter isolates
//! AimDB's per-message contribution.
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_tokio`; results are written
//! to `aimdb-bench/target/bench-results/b0_alloc_tokio.json` (anchored to the
//! crate dir).

use aimdb_bench::{
    alloc::{reset, snapshot},
    profiles::{
        command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
        BATCH_SIZE, WARMUP_ITERS,
    },
    reports::AllocReport,
};
use aimdb_core::buffer::{Buffer, Reader};

fn main() {
    // Current-thread executor — no work-stealing threads, clean allocation signal.
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
        let mut reader = Reader::new(Box::new(buf.subscribe()));

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
        let mut reader = Reader::new(Box::new(buf.subscribe()));

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

    // ── Command: Mailbox / Mutex slot + waker list ───────────────────────────
    //
    // Tight 1:1 push → recv loop. Do NOT batch pushes ahead of the consumer:
    // the single slot overwrites earlier values, leaving only the last write.
    let command_report = rt.block_on(async {
        let buf = command_buffer();
        let mut reader = Reader::new(Box::new(buf.subscribe()));

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
    println!("Expected: 0 allocs/msg — the consume path is allocation-free in steady state.");

    // Persist results for baseline comparison.
    let reports = vec![telemetry_report, state_report, command_report];
    let json = serde_json::to_string_pretty(&reports).expect("failed to serialize reports");
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/bench-results");
    std::fs::create_dir_all(out_dir).expect("failed to create results directory");
    let out_path = format!("{out_dir}/b0_alloc_tokio.json");
    std::fs::write(&out_path, &json).expect("failed to write results");
    println!("\nResults written to {out_path}");
}
