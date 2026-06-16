//! B1 — Push-to-recv latency benchmarks (Criterion).
//!
//! Measures the wall-clock latency from `buf.push(msg)` to `reader.recv()`
//! returning, for each workload profile.  Uses `TokioBuffer<T>` directly to
//! isolate the buffer layer from `AimDb` initialization overhead.
//!
//! **Measurement model:** `iter_custom` gives Criterion the total elapsed time
//! for *iters* push → recv cycles (post-warmup).  Criterion computes the
//! per-iteration p50/p99 distribution over many samples.
//!
//! **Executor:** a single current-thread Tokio runtime is shared across all
//! bench iterations.  This eliminates work-stealing scheduler noise and keeps
//! the signal comparable to the B0 allocator runs.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b1_latency
//! # Save a named baseline:
//! cargo bench -p aimdb-bench --bench b1_latency -- --save-baseline pre-w8
//! # Compare against that baseline:
//! cargo bench -p aimdb-bench --bench b1_latency -- --baseline pre-w8
//! ```

use aimdb_bench::profiles::{
    command_buffer, command_msg, state_buffer, state_msg, telemetry_buffer, telemetry_msg,
    WARMUP_ITERS,
};
use aimdb_core::buffer::{Buffer, BufferReader};
use criterion::{criterion_group, criterion_main, Criterion};

// ── Telemetry: SpmcRing / broadcast ──────────────────────────────────────────

fn bench_latency_telemetry(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B1-Latency");

    group.bench_function("telemetry_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let buf = telemetry_buffer();
                let mut reader = buf.subscribe();

                // Warmup — not timed.
                for i in 0..WARMUP_ITERS {
                    buf.push(telemetry_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(telemetry_msg((WARMUP_ITERS as u64) + i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── State: SingleLatest / watch ───────────────────────────────────────────────

fn bench_latency_state(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B1-Latency");

    group.bench_function("state_spsc", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let buf = state_buffer();
                let mut reader = buf.subscribe();

                for i in 0..WARMUP_ITERS {
                    buf.push(state_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(state_msg((WARMUP_ITERS as u64) + i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

// ── Command: Mailbox / Notify ─────────────────────────────────────────────────

fn bench_latency_command(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("B1-Latency");

    // Tight 1:1 push → recv loop — matches Mailbox semantics.
    group.bench_function("command_mailbox", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let buf = command_buffer();
                let mut reader = buf.subscribe();

                for i in 0..WARMUP_ITERS {
                    buf.push(command_msg(i as u64));
                    let _ = reader.recv().await;
                }

                let start = std::time::Instant::now();
                for i in 0..iters {
                    buf.push(command_msg((WARMUP_ITERS as u64) + i));
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
    bench_latency_telemetry,
    bench_latency_state,
    bench_latency_command,
);
criterion_main!(benches);
