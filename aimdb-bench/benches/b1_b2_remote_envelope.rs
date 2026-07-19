//! Conditional Phase-4 B1/B2 envelope prototype for issue #156.
//!
//! Runs production AimX JSON framing against an isolated native-CBOR consumer
//! for request, write, and subscription-event flows. Transport I/O is excluded.

use std::hint::black_box;
use std::time::Duration;

use aimdb_bench::remote_envelope::{
    encoded_frame_sizes, round_trip, verify_envelope_round_trips, EnvelopeOperation,
    EnvelopeProtocol,
};
use aimdb_bench::remote_ir::{ByteHeavySample, NestedState, NumericTelemetry, RemoteIrFixture};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn benchmark_fixture<T>(criterion: &mut Criterion)
where
    T: RemoteIrFixture,
{
    verify_envelope_round_trips::<T>().expect("envelope fixtures must round-trip");
    println!("\n=== remote envelope sizes: {} ===", T::NAME);
    for (operation, protocol, bytes) in
        encoded_frame_sizes::<T>().expect("envelope sizes must encode")
    {
        println!(
            "{:<8} {:<24} {:>6} bytes",
            operation.name(),
            protocol.name(),
            bytes
        );
    }

    let sample = T::sample();
    let mut group = criterion.benchmark_group(format!("remote_envelope/{}", T::NAME));
    group.throughput(Throughput::Elements(1));

    for operation in EnvelopeOperation::ALL {
        for protocol in EnvelopeProtocol::ALL {
            group.bench_with_input(
                BenchmarkId::new(operation.name(), protocol.name()),
                &(protocol, operation),
                |bencher, &(protocol, operation)| {
                    bencher.iter(|| {
                        let observation = round_trip(protocol, operation, black_box(&sample))
                            .expect("trusted envelope fixture");
                        black_box(observation)
                    });
                },
            );
        }
    }
    group.finish();
}

fn remote_envelope(criterion: &mut Criterion) {
    benchmark_fixture::<NumericTelemetry>(criterion);
    benchmark_fixture::<NestedState>(criterion);
    benchmark_fixture::<ByteHeavySample>(criterion);
}

fn configure() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure();
    targets = remote_envelope
}
criterion_main!(benches);
