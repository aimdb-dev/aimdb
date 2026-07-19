//! B1/B2-Remote-IR — JSON/CBOR latency and throughput for issue #156.
//!
//! Encoded inputs are prepared outside decode timing windows. Each benchmark
//! measures one representation operation, not a complete AimX or connector
//! request.

use std::hint::black_box;
use std::time::Duration;

use aimdb_bench::remote_ir::{
    encoded_sizes, verify_round_trips, ByteHeavySample, NestedState, NumericTelemetry,
    RemoteIrFixture, RemoteIrPath,
};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn bench_fixture<T>(criterion: &mut Criterion)
where
    T: RemoteIrFixture,
{
    verify_round_trips::<T>().expect("all representation paths must round-trip");
    let value = T::sample();

    eprintln!("=== Remote IR payload sizes: {} ===", T::NAME);
    for size in encoded_sizes::<T>().expect("payload-size snapshot") {
        eprintln!("  {:<22} {:>6} bytes", size.path.name(), size.bytes);
    }

    let mut encode_group = criterion.benchmark_group(format!("remote_ir_encode/{}", T::NAME));
    encode_group.throughput(Throughput::Elements(1));
    for path in RemoteIrPath::ALL {
        encode_group.bench_with_input(
            BenchmarkId::from_parameter(path.name()),
            &path,
            |bencher, path| {
                bencher.iter(|| {
                    path.encode(black_box(&value))
                        .expect("fixture encode should succeed")
                });
            },
        );
    }
    encode_group.finish();

    let encoded_inputs = RemoteIrPath::ALL.map(|path| {
        (
            path,
            path.encode(&value).expect("fixture encode should succeed"),
        )
    });

    let mut decode_group = criterion.benchmark_group(format!("remote_ir_decode/{}", T::NAME));
    decode_group.throughput(Throughput::Elements(1));
    for (path, encoded) in &encoded_inputs {
        decode_group.bench_with_input(
            BenchmarkId::from_parameter(path.name()),
            encoded,
            |bencher, encoded| {
                bencher.iter(|| {
                    let decoded: T = path
                        .decode(black_box(encoded.as_slice()))
                        .expect("fixture decode should succeed");
                    black_box(decoded)
                });
            },
        );
    }
    decode_group.finish();
}

fn bench_numeric_telemetry(criterion: &mut Criterion) {
    bench_fixture::<NumericTelemetry>(criterion);
}

fn bench_nested_state(criterion: &mut Criterion) {
    bench_fixture::<NestedState>(criterion);
}

fn bench_byte_heavy(criterion: &mut Criterion) {
    bench_fixture::<ByteHeavySample>(criterion);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_numeric_telemetry, bench_nested_state, bench_byte_heavy
}
criterion_main!(benches);
