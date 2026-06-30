//! B0 — Allocation counting on the Embassy adapter (host-driven).
//!
//! The Embassy companion to [`b0_alloc_tokio`]. Measures per-message allocation
//! cost for each workload profile against the **Embassy** buffer backend
//! ([`EmbassyBuffer`]), driven on the host via `futures::executor::block_on`
//! over embassy-sync's `poll_*` methods — no `embassy-runtime`, no cortex-m
//! executor, no hardware. `poll_recv` drives those methods with no per-message
//! future box, so the expected result is **0 allocs/msg**, same as the Tokio
//! suite; the one-time `Box::new(reader)` and lazy subscriber registration
//! happen during setup/warmup, before the counters are reset.
//!
//! **Measurement model** (identical to `b0_alloc_tokio`): create buffer +
//! reader and **prime** it (forces lazy SpmcRing subscriber registration — see
//! [`profiles_embassy`]), warm up `WARMUP_ITERS` cycles, `reset()`, run
//! `BATCH_SIZE` cycles, then `snapshot()` and divide by `BATCH_SIZE`.
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_embassy`; results are
//! written to `aimdb-bench/target/bench-results/b0_alloc_embassy.json` (anchored
//! to the crate dir).

// The Embassy adapter calls `defmt::*` unconditionally and links embassy-time;
// on the host neither a logger nor a time driver exists. This expands no-op
// stubs so the bench binary links. Must appear exactly once, at top level.
aimdb_embassy_adapter::host_test_stubs!();

use aimdb_bench::{
    alloc::{reset, snapshot},
    profiles::{command_msg, state_msg, telemetry_msg, BATCH_SIZE, WARMUP_ITERS},
    profiles_embassy::{command_buffer, prime, state_buffer, telemetry_buffer},
    reports::AllocReport,
};
use aimdb_core::buffer::{Buffer, Reader};
use futures::executor::block_on;

fn main() {
    println!("=== B0 Allocation Benchmarks (Embassy adapter, buffer layer, host) ===");
    println!("  Warmup iters : {WARMUP_ITERS}");
    println!("  Batch size   : {BATCH_SIZE}");
    println!();

    // ── Telemetry: SpmcRing / PubSubChannel ──────────────────────────────────
    //
    // `prime()` is REQUIRED: the SpmcRing subscriber is created on the reader's
    // first poll, so without it the first pushed message is missed and `recv()`
    // blocks forever.
    let telemetry_report = block_on(async {
        let buf = telemetry_buffer();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        prime(&mut reader);

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

    // ── State: SingleLatest / Watch ──────────────────────────────────────────
    let state_report = block_on(async {
        let buf = state_buffer();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        prime(&mut reader);

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

    // ── Command: Mailbox / Channel(capacity=1) ───────────────────────────────
    //
    // Tight 1:1 push → recv loop matches Mailbox semantics. Do NOT batch pushes
    // ahead of the consumer: the single slot overwrites earlier values.
    let command_report = block_on(async {
        let buf = command_buffer();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        prime(&mut reader);

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
    println!("Expected: 0 allocs/msg — allocation-free consume path, same as the Tokio B0 suite.");

    // Persist results for baseline comparison.
    let reports = vec![telemetry_report, state_report, command_report];
    let json = serde_json::to_string_pretty(&reports).expect("failed to serialize reports");
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/bench-results");
    std::fs::create_dir_all(out_dir).expect("failed to create results directory");
    let out_path = format!("{out_dir}/b0_alloc_embassy.json");
    std::fs::write(&out_path, &json).expect("failed to write results");
    println!("\nResults written to {out_path}");
}
