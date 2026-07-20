//! B1/B2 fan-out encoding — design 048 WI4 (H-B scaling, H-C payoff).
//!
//! One timed iteration is **one broadcast to N subscribers**: turning a single
//! shared record value into N per-subscriber AimX `event` frames. Compares the
//! production per-subscriber codec (`naive`) against the shared-suffix fast
//! path (`shared_suffix`) at N = 1/10/100/500 for a small and a byte-heavy
//! payload. Pure CPU/allocation — no sockets, scheduling, or backpressure.

use std::hint::black_box;
use std::time::Duration;

use aimdb_bench::fanout_encode::{
    encode_naive_into, subscribers, verify_byte_identity, PayloadShape, SharedSuffix, ValidateOnce,
};
use aimdb_core::session::aimx::AimxCodec;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

const SUBSCRIBER_COUNTS: [usize; 4] = [1, 10, 100, 500];

fn bench_shape(criterion: &mut Criterion, shape: PayloadShape) {
    verify_byte_identity(shape)
        .unwrap_or_else(|error| panic!("fast path must match the codec: {error}"));

    let codec = AimxCodec;
    let topic = shape.topic();
    let data = shape.payload();

    // Per-frame size is identical across strategies (byte-identical frames);
    // report it once, outside the timed windows.
    let sample = SharedSuffix::new(topic, data.clone()).encode_one_owned("s0", 1_000_000);
    eprintln!(
        "=== fanout_encode {}: payload {} B, event frame {} B ===",
        shape.name(),
        data.len(),
        sample.len(),
    );

    let mut group = criterion.benchmark_group(format!("fanout_encode/{}", shape.name()));
    for count in SUBSCRIBER_COUNTS {
        let subs = subscribers(count);
        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(BenchmarkId::new("naive", count), &count, |bencher, _| {
            bencher.iter(|| {
                for subscriber in &subs {
                    let mut frame = Vec::new();
                    encode_naive_into(
                        &codec,
                        topic,
                        &data,
                        &subscriber.sub,
                        subscriber.seq,
                        &mut frame,
                    );
                    black_box(frame);
                }
            });
        });

        // Improvement A alone: hoist the record-value validation out of the
        // per-subscriber loop, but keep the serde frame serialization.
        group.bench_with_input(
            BenchmarkId::new("validate_once", count),
            &count,
            |bencher, _| {
                bencher.iter(|| {
                    let validate_once = ValidateOnce::new(&data);
                    for subscriber in &subs {
                        let frame =
                            validate_once.encode_one_owned(topic, &subscriber.sub, subscriber.seq);
                        black_box(frame);
                    }
                });
            },
        );

        // Improvements A + B: shared-suffix fast path (no per-subscriber serde).
        group.bench_with_input(
            BenchmarkId::new("shared_suffix", count),
            &count,
            |bencher, _| {
                bencher.iter(|| {
                    // The per-broadcast precompute (escape `topic` once) is part
                    // of the measured cost, so it lives inside the timed loop.
                    let shared = SharedSuffix::new(topic, data.clone());
                    for subscriber in &subs {
                        let frame = shared.encode_one_owned(&subscriber.sub, subscriber.seq);
                        black_box(frame);
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_small(criterion: &mut Criterion) {
    bench_shape(criterion, PayloadShape::SmallStructured);
}

fn bench_byte_heavy(criterion: &mut Criterion) {
    bench_shape(criterion, PayloadShape::ByteHeavy);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_small, bench_byte_heavy
}
criterion_main!(benches);
