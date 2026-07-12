//! B0-Linkable — allocation gate for the Postcard `Linkable::encode_into` path.
//!
//! This deliberately measures the codec seam, not a complete connector. The
//! core pump still uses a boxed `SerializedReader` future and connector adapters
//! may copy payload ownership; issue #177 only claims that a generated-shape
//! Postcard codec writes into caller-owned storage with zero heap allocations.

use std::hint::black_box;

use aimdb_bench::alloc::{reset, snapshot};
use aimdb_core::connector::LinkCodecError;
use aimdb_data_contracts::{Linkable, SchemaType};
use serde::{Deserialize, Serialize};

const WARMUP_ITERS: usize = 1_000;
const MEASURE_ITERS: usize = 10_000;

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

    fn encode_into(&self, out: &mut [u8]) -> Result<usize, LinkCodecError> {
        match postcard::to_slice(self, out) {
            Ok(used) => Ok(used.len()),
            Err(postcard::Error::SerializeBufferFull) => Err(LinkCodecError::BufferTooSmall),
            Err(_) => Err(LinkCodecError::InvalidData),
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

fn main() {
    let reading = PostcardReading {
        value: 23.75,
        sequence: 42,
    };
    let mut out = [0_u8; 256];

    encode_batch(&reading, &mut out, WARMUP_ITERS);
    reset();
    encode_batch(&reading, &mut out, MEASURE_ITERS);
    let (allocations, bytes) = snapshot();

    println!("=== B0 Linkable Postcard encode_into ===");
    println!("  Iterations  : {MEASURE_ITERS}");
    println!("  Allocations : {allocations}");
    println!("  Bytes       : {bytes}");

    assert_eq!(
        allocations, 0,
        "encode_into allocated in the measured window"
    );
    assert_eq!(
        bytes, 0,
        "encode_into allocated bytes in the measured window"
    );
}
