//! AimDB benchmarking infrastructure. **Not for production use.**
//!
//! Reusable primitives for B0 (allocation counting), B1 (latency), and B2
//! (throughput) benchmarks. The `alloc` module registers
//! [`alloc::CountingAllocator`] as the `#[global_allocator]` for every bench
//! binary that links this crate; nothing in the production dependency graph
//! depends on `aimdb-bench`.
//!
//! # Bench entrypoints
//!
//! | File                              | Class | Purpose                                  |
//! |-----------------------------------|-------|------------------------------------------|
//! | `benches/b0_alloc_tokio.rs`       | B0    | Per-message allocation (Tokio buffer)    |
//! | `benches/b1_b2_tokio.rs`          | B1+B2 | Latency (time/iter) + throughput (Tokio) |
//! | `benches/b0_alloc_embassy.rs`     | B0    | Per-message allocation (Embassy buffer)  |
//! | `benches/b1_b2_embassy.rs`        | B1+B2 | Latency (time/iter) + throughput (Embassy)|
//! | `benches/b_alloc_pipeline.rs`     | info  | Per-message allocation (runner pipeline) |
//! | `benches/b_runner_pipeline.rs`    | info  | Runner pipeline throughput (Criterion)   |
//!
//! On-target cycle profiling (B3) lives in the hardware-only
//! `examples/embassy-bench-stm32h5` crate, since DWT cycle counting cannot run
//! on a host.

pub mod alloc;
pub mod profiles;
pub mod profiles_embassy;
pub mod reports;
