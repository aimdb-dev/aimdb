//! B2 — Steady-state throughput benchmarks (Criterion).
//!
//! Measures messages per second for SPSC (1 producer, 1 consumer) and 1→4
//! fan-out configurations, using `TokioBuffer<T>` directly.
//!
//! **Fan-out safety rules (SpmcRing / broadcast):**
//! - All readers are subscribed *before* any messages are pushed so each
//!   reader holds its read position from the start.
//! - The loop is strict lockstep (1 push, then `recv` on every reader), so at
//!   most one message is ever in flight; the ring capacity (`TELEMETRY_CAPACITY`)
//!   is far more than enough to keep any reader from lagging within an iteration.
//!
//! **Mailbox throughput:** tight 1:1 push → recv loop.  Do NOT batch pushes
//! ahead of the consumer — the single slot overwrites earlier values and
//! only the last write survives, which conflates Mailbox overwrite semantics
//! with throughput measurement.  See design 038 §4 for details.
//!
//! **Executor:** single current-thread Tokio runtime, same as B0/B1.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b2_throughput
//! cargo bench -p aimdb-bench --bench b2_throughput -- --save-baseline pre-w8
//! cargo bench -p aimdb-bench --bench b2_throughput -- --baseline pre-w8
//! ```

use aimdb_bench::profiles::{
    command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
    WARMUP_ITERS,
};
use aimdb_core::buffer::{Buffer, Reader};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};

// ── Telemetry SPSC ────────────────────────────────────────────────────────────

fn bench_throughput_telemetry_spsc(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B2-Throughput");
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                // Subscribe before pushing — reader holds position from start.
                let buf = telemetry_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));

                // Warmup — not timed.
                for i in 0..WARMUP_ITERS {
                    buf.push(telemetry_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(telemetry_msg(i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── Telemetry 1→4 fan-out ────────────────────────────────────────────────────
//
// All 4 readers are subscribed before any messages are pushed.
// Each iteration: 1 push + recv on all 4 readers (sequential in bench, as
// they would all eventually converge on a current-thread executor).
// TELEMETRY_CAPACITY >= BATCH_SIZE ensures no reader lags.

fn bench_throughput_telemetry_fanout(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B2-Throughput");
    // Each iteration produces 1 message observed by 4 consumers.
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_fanout_1x4", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                // All readers subscribed before first push so each holds its
                // read position from the start. Lockstep below keeps at most one
                // message in flight, so the fixed ring capacity never lags.
                let buf = telemetry_buffer();
                let mut r0 = Reader::new(Box::new(buf.subscribe()));
                let mut r1 = Reader::new(Box::new(buf.subscribe()));
                let mut r2 = Reader::new(Box::new(buf.subscribe()));
                let mut r3 = Reader::new(Box::new(buf.subscribe()));

                // Warmup — not timed (mirrors B1 and the SPSC benches).
                for i in 0..WARMUP_ITERS {
                    buf.push(telemetry_msg(i as u64));
                    let _ = r0.recv().await;
                    let _ = r1.recv().await;
                    let _ = r2.recv().await;
                    let _ = r3.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(telemetry_msg(i));
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

// ── State SPSC ────────────────────────────────────────────────────────────────

fn bench_throughput_state_spsc(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B2-Throughput");
    group.throughput(Throughput::Elements(1));

    group.bench_function("state_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let buf = state_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));

                // Warmup — not timed.
                for i in 0..WARMUP_ITERS {
                    buf.push(state_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(state_msg(i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── Command / Mailbox SPSC ────────────────────────────────────────────────────

fn bench_throughput_command_mailbox(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B2-Throughput");
    group.throughput(Throughput::Elements(1));

    group.bench_function("command_mailbox", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let buf = command_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));

                // Warmup — not timed.
                for i in 0..WARMUP_ITERS {
                    buf.push(command_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(command_msg(i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_throughput_telemetry_spsc,
    bench_throughput_telemetry_fanout,
    bench_throughput_state_spsc,
    bench_throughput_command_mailbox,
);
criterion_main!(benches);
