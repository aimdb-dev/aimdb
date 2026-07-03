//! B1/B2 — Latency & throughput benchmarks (Criterion).
//!
//! One Criterion suite capturing **both** measurement classes from a single set
//! of runs, using `TokioBuffer<T>` directly to isolate the buffer layer from
//! `AimDb` initialization overhead:
//!
//! - **B1 latency** — per-iteration time for one `buf.push(msg)` →
//!   `reader.recv()` cycle (the `time` column).
//! - **B2 throughput** — messages/second from that same timing via
//!   `Throughput::Elements(1)` (the `thrpt` column).
//!
//! Covers SPSC (1 producer, 1 consumer) for all three profiles, via
//! [`aimdb_bench::harness::bench_spsc`], plus a 1→4 telemetry fan-out, on a
//! current-thread Tokio runtime (same as B0).
//!
//! **Fan-out safety (SpmcRing / broadcast):** all readers subscribe *before*
//! any push so each holds its read position from the start, and the loop is
//! strict lockstep (1 push, then `recv` on every reader) so at most one message
//! is in flight — `TELEMETRY_CAPACITY` never lags.
//!
//! **Mailbox:** tight 1:1 push → recv loop. Do NOT batch pushes ahead of the
//! consumer — the single slot overwrites earlier values, leaving only the last.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b1_b2_tokio
//! cargo bench -p aimdb-bench --bench b1_b2_tokio -- --save-baseline main
//! cargo bench -p aimdb-bench --bench b1_b2_tokio -- --baseline main
//! ```

use aimdb_bench::harness::{bench_spsc, TokioBlockOn};
use aimdb_bench::profiles::{
    command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
    WARMUP_ITERS,
};
use aimdb_core::buffer::{Buffer, Reader};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};

fn bench_b1_b2_telemetry_spsc(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");
    bench_spsc(
        c,
        "B1-B2",
        "telemetry_spsc",
        telemetry_buffer,
        telemetry_msg,
        &TokioBlockOn(&rt),
    );
}

fn bench_b1_b2_state_spsc(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");
    bench_spsc(
        c,
        "B1-B2",
        "state_spsc",
        state_buffer,
        state_msg,
        &TokioBlockOn(&rt),
    );
}

fn bench_b1_b2_command_mailbox(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");
    bench_spsc(
        c,
        "B1-B2",
        "command_mailbox",
        command_buffer,
        command_msg,
        &TokioBlockOn(&rt),
    );
}

// ── Telemetry 1→4 fan-out ────────────────────────────────────────────────────
//
// Each iteration: 1 push + recv on all 4 readers. Structurally different from
// the SPSC shape above (4 readers, not 1), so it stays bespoke rather than
// forced into `bench_spsc`.

fn bench_b1_b2_telemetry_fanout(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let buf = telemetry_buffer();
    let mut r0 = Reader::new(Box::new(buf.subscribe()));
    let mut r1 = Reader::new(Box::new(buf.subscribe()));
    let mut r2 = Reader::new(Box::new(buf.subscribe()));
    let mut r3 = Reader::new(Box::new(buf.subscribe()));

    // Warmup once, before any Criterion sample.
    rt.block_on(async {
        for i in 0..WARMUP_ITERS {
            buf.push(telemetry_msg(i as u64));
            let _ = r0.recv().await;
            let _ = r1.recv().await;
            let _ = r2.recv().await;
            let _ = r3.recv().await;
        }
    });

    let mut group = c.benchmark_group("B1-B2");
    // Each iteration produces 1 message observed by 4 consumers.
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_fanout_1x4", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
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
