//! B0-Remote-JSON — allocation comparison for issue #196.
//!
//! Measures the real typed-record-to-AimX-envelope in-memory boundary for a
//! canonical `record.get` and one subscription event. Socket I/O, scheduling,
//! and physical transport are deliberately excluded.

use std::hint::black_box;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_bench::remote_json::{verify_remote_json_paths, RemoteJsonHarness, RemoteJsonPath};

const WARMUP_ITERS: usize = 500;
const MEASURE_ITERS: usize = 5_000;
const ALLOCATOR_PROBE_BYTES: usize = 64;

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    RecordGet,
    SubscriptionEvent,
}

impl Operation {
    const ALL: [Self; 2] = [Self::RecordGet, Self::SubscriptionEvent];

    const fn name(self) -> &'static str {
        match self {
            Self::RecordGet => "record_get",
            Self::SubscriptionEvent => "subscription_event",
        }
    }
}

#[derive(Debug)]
struct AllocationRow {
    operation: Operation,
    path: RemoteJsonPath,
    frame_bytes: usize,
    allocations: u64,
    allocated_bytes: u64,
}

fn exercise(
    harness: &mut RemoteJsonHarness,
    path: RemoteJsonPath,
    operation: Operation,
    iterations: usize,
) -> usize {
    let mut frame_bytes = 0;
    for _ in 0..iterations {
        let frame = match operation {
            Operation::RecordGet => harness.record_get_frame(path),
            Operation::SubscriptionEvent => harness.subscription_event_frame(path),
        };
        frame_bytes = black_box(frame.len());
        black_box(frame);
    }
    frame_bytes
}

fn measure(
    harness: &mut RemoteJsonHarness,
    path: RemoteJsonPath,
    operation: Operation,
) -> AllocationRow {
    exercise(harness, path, operation, WARMUP_ITERS);

    reset();
    let frame_bytes = exercise(harness, path, operation, MEASURE_ITERS);
    let (allocations, allocated_bytes) = snapshot();

    AllocationRow {
        operation,
        path,
        frame_bytes,
        allocations,
        allocated_bytes,
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("remote JSON bench runtime");
    runtime.block_on(verify_remote_json_paths());

    let mut rows = Vec::with_capacity(Operation::ALL.len() * 2);
    for operation in Operation::ALL {
        let mut tree = runtime.block_on(RemoteJsonHarness::new());
        rows.push(measure(&mut tree, RemoteJsonPath::Tree, operation));

        let mut direct = runtime.block_on(RemoteJsonHarness::new());
        rows.push(measure(&mut direct, RemoteJsonPath::Direct, operation));
    }

    for operation in Operation::ALL {
        let tree = rows
            .iter()
            .find(|row| row.operation == operation && row.path == RemoteJsonPath::Tree)
            .unwrap();
        let direct = rows
            .iter()
            .find(|row| row.operation == operation && row.path == RemoteJsonPath::Direct)
            .unwrap();

        assert!(
            direct.allocations < tree.allocations,
            "{} direct path did not reduce allocation calls: tree={} direct={}",
            operation.name(),
            tree.allocations,
            direct.allocations
        );
        assert!(
            direct.allocated_bytes < tree.allocated_bytes,
            "{} direct path did not reduce allocated bytes: tree={} direct={}",
            operation.name(),
            tree.allocated_bytes,
            direct.allocated_bytes
        );
    }

    let (probe_allocations, probe_bytes) = allocation_counter_positive_control();

    println!("=== B0 direct JSON production-boundary comparison ===");
    println!("Measured iterations per row: {MEASURE_ITERS}");
    println!("Allocator probe: {probe_allocations} calls / {probe_bytes} bytes");
    println!(
        "{:<20} {:<8} {:>11} {:>12} {:>15} {:>12} {:>14}",
        "operation",
        "path",
        "frame_bytes",
        "allocations",
        "allocated_bytes",
        "allocs/op",
        "bytes/op"
    );

    for row in rows {
        println!(
            "{:<20} {:<8} {:>11} {:>12} {:>15} {:>12.3} {:>14.1}",
            row.operation.name(),
            row.path.name(),
            row.frame_bytes,
            row.allocations,
            row.allocated_bytes,
            row.allocations as f64 / MEASURE_ITERS as f64,
            row.allocated_bytes as f64 / MEASURE_ITERS as f64,
        );
    }
}
