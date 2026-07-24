//! B0 fan-out encoding allocation comparison — design 048 WI4 (H-C).
//!
//! Allocation counts are deterministic, so this is the most reproducible
//! signal for whether the shared-suffix fast path is worth its special-case
//! code. One measured "op" is one broadcast to N subscribers (N frames).
//!
//! Expectation: the naive path allocates per subscriber (a fresh output buffer
//! plus serde intermediates each `encode`); the fast path escapes `topic` once
//! per broadcast, then allocates one output buffer per subscriber.

use std::hint::black_box;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_bench::fanout_encode::{
    encode_naive_into, subscribers, verify_byte_identity, PayloadShape, SharedSuffix, ValidateOnce,
};
use aimdb_core::session::aimx::AimxCodec;

const WARMUP_BROADCASTS: usize = 50;
const MEASURE_BROADCASTS: usize = 500;
const SUBSCRIBER_COUNTS: [usize; 4] = [1, 10, 100, 500];
const ALLOCATOR_PROBE_BYTES: usize = 64;

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

#[derive(Debug)]
struct Row {
    shape: &'static str,
    strategy: &'static str,
    subscribers: usize,
    frame_bytes: usize,
    allocs_per_broadcast: f64,
    allocs_per_frame: f64,
    bytes_per_broadcast: f64,
}

fn run_naive(
    codec: &AimxCodec,
    topic: &str,
    data: &aimdb_core::session::Payload,
    subs: &[aimdb_bench::fanout_encode::Subscriber],
) {
    for subscriber in subs {
        let mut frame = Vec::new();
        encode_naive_into(
            codec,
            topic,
            data,
            &subscriber.sub,
            subscriber.seq,
            &mut frame,
        );
        black_box(&frame);
    }
}

fn run_validate_once(
    topic: &str,
    data: &aimdb_core::session::Payload,
    subs: &[aimdb_bench::fanout_encode::Subscriber],
) {
    let validate_once = ValidateOnce::new(data);
    for subscriber in subs {
        let frame = validate_once.encode_one_owned(topic, &subscriber.sub, subscriber.seq);
        black_box(&frame);
    }
}

fn run_shared(
    topic: &str,
    data: &aimdb_core::session::Payload,
    subs: &[aimdb_bench::fanout_encode::Subscriber],
) {
    let shared = SharedSuffix::new(topic, data.clone());
    for subscriber in subs {
        let frame = shared.encode_one_owned(&subscriber.sub, subscriber.seq);
        black_box(&frame);
    }
}

fn measure(shape: PayloadShape, rows: &mut Vec<Row>) {
    verify_byte_identity(shape)
        .unwrap_or_else(|error| panic!("fast path must match the codec: {error}"));

    let codec = AimxCodec;
    let topic = shape.topic();
    let data = shape.payload();
    let frame_bytes = SharedSuffix::new(topic, data.clone())
        .encode_one_owned("s0", 1_000_000)
        .len();

    for count in SUBSCRIBER_COUNTS {
        let subs = subscribers(count);

        for _ in 0..WARMUP_BROADCASTS {
            run_naive(&codec, topic, &data, &subs);
        }
        reset();
        for _ in 0..MEASURE_BROADCASTS {
            run_naive(&codec, topic, &data, &subs);
        }
        let (allocs, bytes) = snapshot();
        rows.push(row(shape, "naive", count, frame_bytes, allocs, bytes));

        for _ in 0..WARMUP_BROADCASTS {
            run_validate_once(topic, &data, &subs);
        }
        reset();
        for _ in 0..MEASURE_BROADCASTS {
            run_validate_once(topic, &data, &subs);
        }
        let (allocs, bytes) = snapshot();
        rows.push(row(
            shape,
            "validate_once",
            count,
            frame_bytes,
            allocs,
            bytes,
        ));

        for _ in 0..WARMUP_BROADCASTS {
            run_shared(topic, &data, &subs);
        }
        reset();
        for _ in 0..MEASURE_BROADCASTS {
            run_shared(topic, &data, &subs);
        }
        let (allocs, bytes) = snapshot();
        rows.push(row(
            shape,
            "shared_suffix",
            count,
            frame_bytes,
            allocs,
            bytes,
        ));
    }
}

fn row(
    shape: PayloadShape,
    strategy: &'static str,
    subscribers: usize,
    frame_bytes: usize,
    allocs: u64,
    bytes: u64,
) -> Row {
    let broadcasts = MEASURE_BROADCASTS as f64;
    Row {
        shape: shape.name(),
        strategy,
        subscribers,
        frame_bytes,
        allocs_per_broadcast: allocs as f64 / broadcasts,
        allocs_per_frame: allocs as f64 / broadcasts / subscribers as f64,
        bytes_per_broadcast: bytes as f64 / broadcasts,
    }
}

/// Prove this bench binary's global allocator reaches the shared counters.
fn allocation_counter_positive_control() -> (u64, u64) {
    reset();
    let mut probe = Vec::<u8>::with_capacity(black_box(ALLOCATOR_PROBE_BYTES));
    probe.push(black_box(0xA5));
    black_box(probe.as_slice());
    let measured = snapshot();
    assert!(
        measured.0 >= 1,
        "allocation counter missed the deliberate Vec"
    );
    drop(probe);
    measured
}

fn main() {
    let mut rows = Vec::new();
    measure(PayloadShape::SmallStructured, &mut rows);
    measure(PayloadShape::ByteHeavy, &mut rows);
    let (probe_allocs, probe_bytes) = allocation_counter_positive_control();

    println!("=== B0 fan-out encoding allocation comparison ===");
    println!("Broadcasts per row: {MEASURE_BROADCASTS}");
    println!("Allocator probe: {probe_allocs} calls / {probe_bytes} bytes");
    println!(
        "{:<18} {:<14} {:>5} {:>11} {:>16} {:>14} {:>16}",
        "shape",
        "strategy",
        "subs",
        "frame_B",
        "allocs/broadcast",
        "allocs/frame",
        "bytes/broadcast"
    );
    for row in rows {
        println!(
            "{:<18} {:<14} {:>5} {:>11} {:>16.1} {:>14.3} {:>16.1}",
            row.shape,
            row.strategy,
            row.subscribers,
            row.frame_bytes,
            row.allocs_per_broadcast,
            row.allocs_per_frame,
            row.bytes_per_broadcast,
        );
    }
}
