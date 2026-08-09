//! B1/B2-Remote-JSON — latency and throughput for issue #196.
//!
//! Compares the compatibility JSON-tree route with direct JSON bytes through
//! the real typed record, runtime buffer, opaque payload, and AimX envelope.
//! Transport I/O and scheduler wakeups are outside the measured window.

use std::hint::black_box;
use std::time::Duration;

use aimdb_bench::remote_json::{verify_remote_json_paths, RemoteJsonHarness, RemoteJsonPath};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn remote_json(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("remote JSON bench runtime");
    runtime.block_on(verify_remote_json_paths());

    let mut group = criterion.benchmark_group("remote_json");
    group.throughput(Throughput::Elements(1));

    let mut tree_get = runtime.block_on(RemoteJsonHarness::new());
    group.bench_function(
        BenchmarkId::new("record_get", RemoteJsonPath::Tree.name()),
        |bencher| {
            bencher.iter(|| {
                let frame = tree_get.record_get_frame(RemoteJsonPath::Tree);
                black_box(frame);
            });
        },
    );

    let mut direct_get = runtime.block_on(RemoteJsonHarness::new());
    group.bench_function(
        BenchmarkId::new("record_get", RemoteJsonPath::Direct.name()),
        |bencher| {
            bencher.iter(|| {
                let frame = direct_get.record_get_frame(RemoteJsonPath::Direct);
                black_box(frame);
            });
        },
    );

    let mut tree_event = runtime.block_on(RemoteJsonHarness::new());
    group.bench_function(
        BenchmarkId::new("subscription_event", RemoteJsonPath::Tree.name()),
        |bencher| {
            bencher.iter(|| {
                let frame = tree_event.subscription_event_frame(RemoteJsonPath::Tree);
                black_box(frame);
            });
        },
    );

    let mut direct_event = runtime.block_on(RemoteJsonHarness::new());
    group.bench_function(
        BenchmarkId::new("subscription_event", RemoteJsonPath::Direct.name()),
        |bencher| {
            bencher.iter(|| {
                let frame = direct_event.subscription_event_frame(RemoteJsonPath::Direct);
                black_box(frame);
            });
        },
    );

    group.finish();
}

fn configure() -> Criterion {
    Criterion::default()
        .sample_size(40)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure();
    targets = remote_json
}
criterion_main!(benches);
