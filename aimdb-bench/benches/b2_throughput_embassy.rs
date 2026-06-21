//! B2 — Steady-state throughput on the Embassy adapter (host-driven, Criterion).
//!
//! The Embassy companion to [`b2_throughput`]. Measures messages per second for
//! SPSC (1 producer, 1 consumer) and 1→4 fan-out configurations against the
//! **Embassy** buffer backend, driven on the host via
//! `futures::executor::block_on` — no `embassy-runtime`, no cortex-m executor,
//! no hardware.
//!
//! These are host throughput numbers for trend tracking and Tokio-vs-Embassy
//! comparison; on-target throughput in CPU cycles is covered by the B3 STM32H5
//! bench (`examples/embassy-bench-stm32h5`).
//!
//! **Fan-out safety rules (SpmcRing / PubSubChannel):**
//! - All readers are **primed** before any messages are pushed, so each holds
//!   its read position from the start (the embassy `Subscriber` is otherwise
//!   created lazily on first poll and would miss earlier messages).
//! - `SUBS = 4` on [`TelemetryBuffer`](aimdb_bench::profiles_embassy::TelemetryBuffer)
//!   provides exactly four subscriber slots for the fan-out.
//! - The loop is strict lockstep (1 push, then `recv` on every reader), so at
//!   most one message is ever in flight and the fixed `CAP` never lags.
//!
//! **Mailbox throughput:** tight 1:1 push → recv loop. Do NOT batch pushes
//! ahead of the consumer — the single slot overwrites earlier values.
//!
//! Run:
//! ```text
//! cargo bench -p aimdb-bench --bench b2_throughput_embassy
//! cargo bench -p aimdb-bench --bench b2_throughput_embassy -- --save-baseline main
//! cargo bench -p aimdb-bench --bench b2_throughput_embassy -- --baseline main
//! ```

aimdb_embassy_adapter::host_test_stubs!();

use aimdb_bench::profiles::{command_msg, state_msg, telemetry_msg, WARMUP_ITERS};
use aimdb_bench::profiles_embassy::{command_buffer, prime, state_buffer, telemetry_buffer};
use aimdb_core::buffer::{Buffer, Reader};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use futures::executor::block_on;

// ── Telemetry SPSC ────────────────────────────────────────────────────────────

fn bench_throughput_telemetry_spsc(c: &mut Criterion) {
    let mut group = c.benchmark_group("B2-Throughput-Embassy");
    group.throughput(Throughput::Elements(1));

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
// All 4 readers are primed before any messages are pushed, so each registers
// its subscriber at the current position. Each iteration: 1 push + recv on all
// 4 readers. Lockstep keeps at most one message in flight, so the fixed CAP
// never lags.

fn bench_throughput_telemetry_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("B2-Throughput-Embassy");
    // Each iteration produces 1 message observed by 4 consumers.
    group.throughput(Throughput::Elements(1));

    group.bench_function("telemetry_fanout_1x4", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = telemetry_buffer();
                let mut r0 = Reader::new(Box::new(buf.subscribe()));
                let mut r1 = Reader::new(Box::new(buf.subscribe()));
                let mut r2 = Reader::new(Box::new(buf.subscribe()));
                let mut r3 = Reader::new(Box::new(buf.subscribe()));
                // Prime all four BEFORE the first push (registers 4 subscribers).
                prime(&mut r0);
                prime(&mut r1);
                prime(&mut r2);
                prime(&mut r3);

                // Warmup — not timed.
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
    let mut group = c.benchmark_group("B2-Throughput-Embassy");
    group.throughput(Throughput::Elements(1));

    group.bench_function("state_spsc", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = state_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));
                prime(&mut reader);

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
    let mut group = c.benchmark_group("B2-Throughput-Embassy");
    group.throughput(Throughput::Elements(1));

    group.bench_function("command_mailbox", |b| {
        b.iter_custom(|iters| {
            block_on(async {
                let buf = command_buffer();
                let mut reader = Reader::new(Box::new(buf.subscribe()));
                prime(&mut reader);

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
