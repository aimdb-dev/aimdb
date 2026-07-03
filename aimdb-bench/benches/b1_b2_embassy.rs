//! B1/B2 — Latency & throughput on the Embassy adapter (host-driven, Criterion).
//!
//! The Embassy companion to [`b1_b2_tokio`], capturing **both** measurement
//! classes from one set of runs against the **Embassy** buffer backend, driven
//! on the host via `futures::executor::block_on` — no `embassy-runtime`, no
//! cortex-m executor, no hardware:
//!
//! - **B1 latency** — per-iteration time for one `buf.push(msg)` →
//!   `reader.recv()` cycle (the `time` column).
//! - **B2 throughput** — messages/second from that same timing via
//!   `Throughput::Elements(1)` (the `thrpt` column).
//!
//! Covers SPSC (1 producer, 1 consumer) for all three profiles, via
//! [`aimdb_bench::harness::bench_spsc`], plus a 1→4 telemetry fan-out. These
//! are host wall-clock numbers for trend tracking and Tokio-vs-Embassy
//! comparison; on-target cycle counts are covered by the B3 STM32H5 bench
//! (`examples/embassy-bench-stm32h5`).
//!
//! **Fan-out safety (SpmcRing / PubSubChannel):** each reader's embassy
//! `Subscriber` is registered eagerly at `subscribe()` time, so every
//! reader holds its read position from the start with no
//! separate priming step; `SUBS = 4` on
//! [`TelemetryBuffer`](aimdb_bench::profiles_embassy::TelemetryBuffer) provides
//! the four subscriber slots, and strict lockstep keeps the fixed `CAP` from
//! lagging.
//!
//! **Mailbox:** tight 1:1 push → recv loop. Do NOT batch pushes ahead of the
//! consumer — the single slot overwrites earlier values.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b1_b2_embassy
//! cargo bench -p aimdb-bench --bench b1_b2_embassy -- --save-baseline main
//! cargo bench -p aimdb-bench --bench b1_b2_embassy -- --baseline main
//! ```

aimdb_embassy_adapter::host_test_stubs!();

use aimdb_bench::harness::{bench_spsc, FuturesBlockOn};
use aimdb_bench::profiles::{command_msg, state_msg, telemetry_msg, WARMUP_ITERS};
use aimdb_bench::profiles_embassy::{command_buffer, state_buffer, telemetry_buffer};
use aimdb_core::buffer::{Buffer, Reader};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use futures::executor::block_on;

const GROUP: &str = "B1-B2-Embassy";

fn bench_b1_b2_telemetry_spsc(c: &mut Criterion) {
    bench_spsc(
        c,
        GROUP,
        "telemetry_spsc",
        telemetry_buffer,
        telemetry_msg,
        &FuturesBlockOn,
    );
}

fn bench_b1_b2_state_spsc(c: &mut Criterion) {
    bench_spsc(
        c,
        GROUP,
        "state_spsc",
        state_buffer,
        state_msg,
        &FuturesBlockOn,
    );
}

fn bench_b1_b2_command_mailbox(c: &mut Criterion) {
    bench_spsc(
        c,
        GROUP,
        "command_mailbox",
        command_buffer,
        command_msg,
        &FuturesBlockOn,
    );
}

// ── Telemetry 1→4 fan-out ────────────────────────────────────────────────────
//
// Each iteration: 1 push + recv on all 4 readers. Structurally different from
// the SPSC shape above (4 readers, not 1), so it stays bespoke rather than
// forced into `bench_spsc`.

fn bench_b1_b2_telemetry_fanout(c: &mut Criterion) {
    let buf = telemetry_buffer();
    let mut r0 = Reader::new(Box::new(buf.subscribe()));
    let mut r1 = Reader::new(Box::new(buf.subscribe()));
    let mut r2 = Reader::new(Box::new(buf.subscribe()));
    let mut r3 = Reader::new(Box::new(buf.subscribe()));

    // Warmup once, before any Criterion sample.
    block_on(async {
        for i in 0..WARMUP_ITERS {
            buf.push(telemetry_msg(i as u64));
            let _ = r0.recv().await;
            let _ = r1.recv().await;
            let _ = r2.recv().await;
            let _ = r3.recv().await;
        }
    });

    let mut group = c.benchmark_group(GROUP);
    // Each iteration produces 1 message observed by 4 consumers.
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_fanout_1x4", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(telemetry_msg((WARMUP_ITERS as u64) + i));
                    let _ = r0.recv().await;
                    let _ = r1.recv().await;
                    let _ = r2.recv().await;
                    let _ = r3.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_b1_b2_telemetry_spsc,
    bench_b1_b2_telemetry_fanout,
    bench_b1_b2_state_spsc,
    bench_b1_b2_command_mailbox,
);
criterion_main!(benches);
