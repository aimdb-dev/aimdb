//! Shared measurement harness for the B0/B1/B2 host bench suites —
//! collapses the near-identical setup/warmup/measure shape that used
//! to be hand-copied across `b0_alloc_{tokio,embassy}.rs` and
//! `b1_b2_{tokio,embassy}.rs`.

use std::future::Future;
use std::time::Instant;

use aimdb_core::buffer::{Buffer, Reader};
use criterion::{Criterion, Throughput};

use crate::payloads::{BATCH_SIZE, WARMUP_ITERS};
use crate::reports::AllocReport;

/// Runs a future to completion on whichever executor a bench file is using
/// (tokio vs `futures::executor`), so [`bench_spsc`] doesn't need to know
/// which one it's given. A `dyn`-unfriendly generic method (not a trait
/// object) — each call site monomorphizes, so this adds no per-call
/// allocation.
pub trait BlockOn {
    fn block_on<F: Future>(&self, fut: F) -> F::Output;
}

/// [`BlockOn`] over a [`tokio::runtime::Runtime`] (current-thread, per the
/// existing B0-B2 noise-reduction rationale).
pub struct TokioBlockOn<'a>(pub &'a tokio::runtime::Runtime);

impl BlockOn for TokioBlockOn<'_> {
    fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.0.block_on(fut)
    }
}

/// [`BlockOn`] over `futures::executor::block_on`, for the Embassy host
/// suites (no tokio runtime involved).
pub struct FuturesBlockOn;

impl BlockOn for FuturesBlockOn {
    fn block_on<F: Future>(&self, fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }
}

/// Subscribe, warm up `WARMUP_ITERS` push→recv cycles, reset the allocation
/// counters, then run `BATCH_SIZE` more cycles and report the result —
/// the B0 measurement model shared by every workload profile on both
/// adapters. Async: the caller `block_on`s it with whichever executor
/// matches their adapter.
pub async fn measure_b0<T, B>(
    buf: &B,
    mk_msg: impl Fn(u64) -> T,
    profile: &str,
    buffer_type: &str,
) -> AllocReport
where
    B: Buffer<T>,
    T: Clone + Send,
{
    let mut reader = Reader::new(Box::new(buf.subscribe()));

    for i in 0..WARMUP_ITERS {
        buf.push(mk_msg(i as u64));
        let _ = reader.recv().await;
    }

    crate::alloc::reset();
    for i in 0..BATCH_SIZE {
        buf.push(mk_msg((WARMUP_ITERS + i) as u64));
        let _ = reader.recv().await;
    }
    let (allocs, bytes) = crate::alloc::snapshot();
    AllocReport::new(profile, buffer_type, BATCH_SIZE, allocs, bytes)
}

/// Registers a Criterion SPSC (1 producer, 1 consumer) B1/B2 bench under
/// `group_name` — the setup/warmup/measure shape shared by every SPSC
/// profile on both adapters (the tokio suite uses group `"B1-B2"`, the
/// Embassy suite `"B1-B2-Embassy"` — kept distinct so existing
/// `--save-baseline`/`--baseline` comparisons keep working). Warmup runs
/// **once**, before Criterion's first sample, not on every `iter_custom`
/// call (redoing it per sample was wasteful and could
/// itself perturb the steady-state numbers via repeated allocator/cache
/// warm-state resets).
pub fn bench_spsc<T, B>(
    c: &mut Criterion,
    group_name: &str,
    name: &str,
    mk_buf: impl Fn() -> B,
    mk_msg: impl Fn(u64) -> T + Copy,
    executor: &impl BlockOn,
) where
    B: Buffer<T>,
    T: Clone + Send,
{
    let buf = mk_buf();
    let mut reader = Reader::new(Box::new(buf.subscribe()));

    executor.block_on(async {
        for i in 0..WARMUP_ITERS {
            buf.push(mk_msg(i as u64));
            let _ = reader.recv().await;
        }
    });

    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Elements(1));
    group.bench_function(name, |b| {
        b.iter_custom(|iters| {
            executor.block_on(async {
                let start = Instant::now();
                for i in 0..iters {
                    buf.push(mk_msg(WARMUP_ITERS as u64 + i));
                    let _ = reader.recv().await;
                }
                start.elapsed()
            })
        });
    });
    group.finish();
}
