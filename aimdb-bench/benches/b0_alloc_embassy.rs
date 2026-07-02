//! B0 — Allocation counting on the Embassy adapter (host-driven).
//!
//! The Embassy companion to [`b0_alloc_tokio`]. Measures per-message allocation
//! cost for each workload profile against the **Embassy** buffer backend
//! ([`EmbassyBuffer`]), driven on the host via `futures::executor::block_on`
//! over embassy-sync's `poll_*` methods — no `embassy-runtime`, no cortex-m
//! executor, no hardware. `poll_recv` drives those methods with no per-message
//! future box, so the expected result is **0 allocs/msg**, same as the Tokio
//! suite; the one-time `Box::new(reader)` and (eager, at `subscribe()` —
//! design 039 F9) subscriber registration happen during setup/warmup, before
//! the counters are reset.
//!
//! **Measurement model** (identical to `b0_alloc_tokio`, via
//! [`aimdb_bench::harness::measure_b0`]): create buffer + reader, warm up
//! `WARMUP_ITERS` cycles, `reset()`, run `BATCH_SIZE` cycles, then
//! `snapshot()` and divide by `BATCH_SIZE`.
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_embassy`; results are
//! written to `aimdb-bench/target/bench-results/b0_alloc_embassy.json` (anchored
//! to the crate dir).

// The Embassy adapter calls `defmt::*` unconditionally and links embassy-time;
// on the host neither a logger nor a time driver exists. This expands no-op
// stubs so the bench binary links. Must appear exactly once, at top level.
aimdb_embassy_adapter::host_test_stubs!();

use aimdb_bench::{
    harness::measure_b0,
    profiles::{command_msg, state_msg, telemetry_msg, BATCH_SIZE, WARMUP_ITERS},
    profiles_embassy::{command_buffer, state_buffer, telemetry_buffer},
    reports::write_reports,
};
use futures::executor::block_on;

// One `#[global_allocator]` per bench binary (design 039 F12).
#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

fn main() {
    println!("=== B0 Allocation Benchmarks (Embassy adapter, buffer layer, host) ===");
    println!("  Warmup iters : {WARMUP_ITERS}");
    println!("  Batch size   : {BATCH_SIZE}");
    println!();

    let telemetry_report = block_on(async {
        measure_b0(&telemetry_buffer(), telemetry_msg, "Telemetry", "SpmcRing").await
    });
    telemetry_report.print();

    let state_report =
        block_on(async { measure_b0(&state_buffer(), state_msg, "State", "SingleLatest").await });
    state_report.print();

    // Tight 1:1 push → recv loop for Mailbox. Do NOT batch pushes ahead of the
    // consumer: the single slot overwrites earlier values. `measure_b0`
    // already does this (lockstep push/recv).
    let command_report =
        block_on(async { measure_b0(&command_buffer(), command_msg, "Command", "Mailbox").await });
    command_report.print();

    println!();
    println!("Expected: 0 allocs/msg — allocation-free consume path, same as the Tokio B0 suite.");

    write_reports(
        "b0_alloc_embassy",
        &[telemetry_report, state_report, command_report],
    );
}
