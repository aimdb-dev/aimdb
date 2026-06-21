//! B1 — Push-to-recv latency on the Embassy adapter (host-driven, Criterion).
//!
//! The Embassy companion to [`b1_latency`]. Measures the wall-clock latency
//! from `buf.push(msg)` to `reader.recv()` returning, for each workload
//! profile, against the **Embassy** buffer backend. Driven on the host via
//! `futures::executor::block_on` over embassy-sync's poll methods — no
//! `embassy-runtime`, no cortex-m executor, no hardware.
//!
//! These are host wall-clock numbers for trend tracking and Tokio-vs-Embassy
//! comparison; they are **not** a substitute for on-target cycle counts. Real
//! embedded latency is measured in CPU cycles by the B3 STM32H5 bench
//! (`examples/embassy-bench-stm32h5`).
//!
//! **Measurement model:** `iter_custom` gives Criterion the total elapsed time
//! for *iters* push → recv cycles (post-warmup). Each reader is **primed**
//! before the first push so the lazy SpmcRing subscriber is registered (see
//! [`profiles_embassy`]).
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b1_latency_embassy
//! cargo bench -p aimdb-bench --bench b1_latency_embassy -- --save-baseline pre-x
//! cargo bench -p aimdb-bench --bench b1_latency_embassy -- --baseline pre-x
//! ```

aimdb_embassy_adapter::host_test_stubs!();

use aimdb_bench::profiles::{command_msg, state_msg, telemetry_msg, WARMUP_ITERS};
use aimdb_bench::profiles_embassy::{command_buffer, prime, state_buffer, telemetry_buffer};
use aimdb_core::buffer::{Buffer, Reader};
use criterion::{criterion_group, criterion_main, Criterion};
use futures::executor::block_on;

// ── Telemetry: SpmcRing / PubSubChannel ──────────────────────────────────────

fn bench_latency_telemetry(c: &mut Criterion) {
    let mut group = c.benchmark_group("B1-Latency-Embassy");

    group.bench_function("telemetry_spsc", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = telemetry_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));
                prime(&mut reader);

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

// ── State: SingleLatest / Watch ───────────────────────────────────────────────

fn bench_latency_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("B1-Latency-Embassy");

    group.bench_function("state_spsc", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = state_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));
                prime(&mut reader);

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

// ── Command: Mailbox / Channel(capacity=1) ────────────────────────────────────

fn bench_latency_command(c: &mut Criterion) {
    let mut group = c.benchmark_group("B1-Latency-Embassy");

    // Tight 1:1 push → recv loop — matches Mailbox semantics.
    group.bench_function("command_mailbox", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = command_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));
                prime(&mut reader);

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
