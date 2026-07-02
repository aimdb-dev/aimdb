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
//! `snapshot()` and divide by `BATCH_SIZE` (see [`aimdb_bench::harness::measure_b0`]).
//! A current-thread Tokio runtime keeps scheduler allocation out of the hot
//! path so the counter isolates AimDB's per-message contribution.
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_tokio`; results are written
//! to `aimdb-bench/target/bench-results/b0_alloc_tokio.json` (anchored to the
//! crate dir).

use aimdb_bench::{
    harness::measure_b0,
    profiles::{
        command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
        BATCH_SIZE, WARMUP_ITERS,
    },
    reports::write_reports,
};

// One `#[global_allocator]` per bench binary (design 039 F12) — `alloc.rs`
// itself no longer declares one, so each binary picks its own inner
// allocator (here, `std::alloc::System`).
#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

fn main() {
    // Current-thread executor — no work-stealing threads, clean allocation signal.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("failed to build current-thread Tokio runtime");

    println!("=== B0 Allocation Benchmarks (Tokio adapter, buffer layer) ===");
    println!("  Warmup iters : {WARMUP_ITERS}");
    println!("  Batch size   : {BATCH_SIZE}");
    println!();

    // Subscribe before pushing so the reader holds its read position from the
    // start — a reader created after sends are in flight misses them.
    let telemetry_report = rt.block_on(async {
        measure_b0(&telemetry_buffer(), telemetry_msg, "Telemetry", "SpmcRing").await
    });
    telemetry_report.print();

    let state_report = rt
        .block_on(async { measure_b0(&state_buffer(), state_msg, "State", "SingleLatest").await });
    state_report.print();

    // Tight 1:1 push → recv loop for Mailbox. Do NOT batch pushes ahead of the
    // consumer: the single slot overwrites earlier values, leaving only the
    // last write. `measure_b0` already does this (lockstep push/recv).
    let command_report = rt
        .block_on(async { measure_b0(&command_buffer(), command_msg, "Command", "Mailbox").await });
    command_report.print();

    println!();
    println!("Expected: 0 allocs/msg — the consume path is allocation-free in steady state.");

    write_reports(
        "b0_alloc_tokio",
        &[telemetry_report, state_report, command_report],
    );
}
