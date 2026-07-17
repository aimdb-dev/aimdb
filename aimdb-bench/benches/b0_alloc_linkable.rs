//! B0-Linkable — allocation gate for direct and per-link Postcard encoding.
//!
//! This deliberately measures the codec seam, not a complete connector. The
//! core pump still uses a boxed `SerializedReader` future and connector adapters
//! may copy payload ownership; issue #177 only claims that a generated-shape
//! Postcard codec writes into caller-owned storage with zero heap allocations.

use std::hint::black_box;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_core::connector::SerializeError;
use aimdb_data_contracts::{link_codecs, LinkCodec, Linkable, SchemaType};
use serde::{Deserialize, Serialize};

const WARMUP_ITERS: usize = 1_000;
const MEASURE_ITERS: usize = 10_000;
const ALLOCATOR_PROBE_BYTES: usize = 64;

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PostcardReading {
    value: f32,
    sequence: u32,
}

impl SchemaType for PostcardReading {
    const NAME: &'static str = "postcard_reading";
}

impl Linkable for PostcardReading {
    const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(256);

    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        postcard::from_bytes(data).map_err(|e| e.to_string())
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(self).map_err(|e| e.to_string())
    }

    fn encode_into(&self, out: &mut [u8]) -> Result<usize, SerializeError> {
        match postcard::to_slice(self, out) {
            Ok(used) => Ok(used.len()),
            Err(postcard::Error::SerializeBufferFull) => Err(SerializeError::BufferTooSmall),
            Err(_) => Err(SerializeError::InvalidData),
        }
    }
}

fn encode_batch(reading: &PostcardReading, out: &mut [u8; 256], iterations: usize) {
    for _ in 0..iterations {
        let written = black_box(reading)
            .encode_into(black_box(out.as_mut_slice()))
            .expect("Postcard encode_into should fit the fixed scratch buffer");
        black_box(&out[..written]);
    }
}

fn encode_per_link_codec_batch(reading: &PostcardReading, out: &mut [u8; 256], iterations: usize) {
    let codec = link_codecs::Postcard::<256>;
    for _ in 0..iterations {
        let written = codec
            .encode_into(black_box(reading), black_box(out.as_mut_slice()))
            .expect("per-link Postcard encode_into should fit the fixed scratch buffer");
        black_box(&out[..written]);
    }
}

/// Prove that this binary's global allocator is actually wired through the
/// counters. Run after the measured windows so the deliberate allocation
/// cannot contaminate either zero-allocation result.
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
    let reading = PostcardReading {
        value: 23.75,
        sequence: 42,
    };
    let mut out = [0_u8; 256];

    encode_batch(&reading, &mut out, WARMUP_ITERS);
    reset();
    encode_batch(&reading, &mut out, MEASURE_ITERS);
    let (direct_allocations, direct_bytes) = snapshot();

    encode_per_link_codec_batch(&reading, &mut out, WARMUP_ITERS);
    reset();
    encode_per_link_codec_batch(&reading, &mut out, MEASURE_ITERS);
    let (codec_allocations, codec_bytes) = snapshot();

    // This resets the global counters only after both result tuples have been
    // captured, then deliberately allocates to reject a disconnected or broken
    // counting allocator that would otherwise report a misleading zero.
    let (probe_allocations, probe_bytes) = allocation_counter_positive_control();

    println!("=== B0 Postcard encode_into ===");
    println!("  Iterations  : {MEASURE_ITERS}");
    println!("  Allocator probe allocations : {probe_allocations}");
    println!("  Allocator probe bytes       : {probe_bytes}");
    println!("  Direct Linkable allocations : {direct_allocations}");
    println!("  Direct Linkable bytes       : {direct_bytes}");
    println!("  Per-link codec allocations  : {codec_allocations}");
    println!("  Per-link codec bytes        : {codec_bytes}");

    assert_eq!(
        direct_allocations, 0,
        "direct Linkable::encode_into allocated in the measured window"
    );
    assert_eq!(
        direct_bytes, 0,
        "direct Linkable::encode_into allocated bytes in the measured window"
    );
    assert_eq!(
        codec_allocations, 0,
        "per-link Postcard codec allocated in the measured window"
    );
    assert_eq!(
        codec_bytes, 0,
        "per-link Postcard codec allocated bytes in the measured window"
    );
}
