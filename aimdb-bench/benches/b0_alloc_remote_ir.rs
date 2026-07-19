//! B0-Remote-IR — isolated allocation comparison for issue #156.
//!
//! This measures representation work only. It does not include AimX framing,
//! connector copies, scheduler wakeups, transport I/O, or backpressure.

use std::hint::black_box;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_bench::remote_ir::{
    verify_round_trips, ByteHeavySample, NestedState, NumericTelemetry, RemoteIrFixture,
    RemoteIrPath,
};

const WARMUP_ITERS: usize = 200;
const MEASURE_ITERS: usize = 1_000;
const ALLOCATOR_PROBE_BYTES: usize = 64;

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

#[derive(Debug)]
struct AllocationRow {
    shape: &'static str,
    operation: &'static str,
    path: &'static str,
    payload_bytes: usize,
    allocations: u64,
    allocated_bytes: u64,
}

fn warm_encode<T>(path: RemoteIrPath, value: &T, iterations: usize)
where
    T: RemoteIrFixture,
{
    for _ in 0..iterations {
        let encoded = path
            .encode(black_box(value))
            .expect("fixture encode should succeed");
        black_box(encoded.as_slice());
    }
}

fn warm_decode<T>(path: RemoteIrPath, encoded: &[u8], iterations: usize)
where
    T: RemoteIrFixture,
{
    for _ in 0..iterations {
        let decoded: T = path
            .decode(black_box(encoded))
            .expect("fixture decode should succeed");
        black_box(decoded);
    }
}

fn measure_fixture<T>(rows: &mut Vec<AllocationRow>)
where
    T: RemoteIrFixture,
{
    verify_round_trips::<T>().expect("all representation paths must round-trip");
    let value = T::sample();

    for path in RemoteIrPath::ALL {
        let encoded = path.encode(&value).expect("fixture encode should succeed");
        let payload_bytes = encoded.len();

        warm_encode(path, &value, WARMUP_ITERS);
        reset();
        warm_encode(path, &value, MEASURE_ITERS);
        let (allocations, allocated_bytes) = snapshot();
        rows.push(AllocationRow {
            shape: T::NAME,
            operation: "encode",
            path: path.name(),
            payload_bytes,
            allocations,
            allocated_bytes,
        });

        warm_decode::<T>(path, &encoded, WARMUP_ITERS);
        reset();
        warm_decode::<T>(path, &encoded, MEASURE_ITERS);
        let (allocations, allocated_bytes) = snapshot();
        rows.push(AllocationRow {
            shape: T::NAME,
            operation: "decode",
            path: path.name(),
            payload_bytes,
            allocations,
            allocated_bytes,
        });
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
        "allocation counter missed the deliberate Vec allocation"
    );
    assert!(
        measured.1 >= ALLOCATOR_PROBE_BYTES as u64,
        "byte counter missed the deliberate Vec capacity"
    );

    drop(probe);
    measured
}

fn main() {
    let mut rows = Vec::with_capacity(3 * RemoteIrPath::ALL.len() * 2);
    measure_fixture::<NumericTelemetry>(&mut rows);
    measure_fixture::<NestedState>(&mut rows);
    measure_fixture::<ByteHeavySample>(&mut rows);

    // Run after every measurement tuple is captured so the deliberate
    // allocation cannot contaminate any representation window.
    let (probe_allocations, probe_bytes) = allocation_counter_positive_control();

    println!("=== B0 Remote IR allocation comparison ===");
    println!("Measured iterations per row: {MEASURE_ITERS}");
    println!("Allocator probe: {probe_allocations} calls / {probe_bytes} bytes");
    println!(
        "{:<20} {:<7} {:<22} {:>9} {:>12} {:>15} {:>12} {:>14}",
        "shape", "op", "path", "payload", "allocations", "allocated_bytes", "allocs/op", "bytes/op"
    );

    for row in rows {
        println!(
            "{:<20} {:<7} {:<22} {:>9} {:>12} {:>15} {:>12.3} {:>14.1}",
            row.shape,
            row.operation,
            row.path,
            row.payload_bytes,
            row.allocations,
            row.allocated_bytes,
            row.allocations as f64 / MEASURE_ITERS as f64,
            row.allocated_bytes as f64 / MEASURE_ITERS as f64,
        );
    }
}
