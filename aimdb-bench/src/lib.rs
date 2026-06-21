//! AimDB benchmarking infrastructure.
//!
//! Provides reusable primitives for B0 (allocation counting), B1 (latency),
//! and B2 (throughput) benchmarks.  **Not for production use.**
//!
//! The `alloc` module registers [`alloc::CountingAllocator`] as the
//! `#[global_allocator]` for every bench binary that links this crate.
//! Nothing in the production dependency graph depends on `aimdb-bench`.
//!
//! # Bench entrypoints
//!
//! | File                              | Class | Purpose                                  |
//! |-----------------------------------|-------|------------------------------------------|
//! | `benches/b0_alloc_tokio.rs`       | B0    | Per-message allocation (Tokio buffer)    |
//! | `benches/b1_latency.rs`           | B1    | Push-to-recv latency (Tokio buffer)      |
//! | `benches/b2_throughput.rs`        | B2    | Steady-state throughput (Tokio buffer)   |
//! | `benches/b0_alloc_embassy.rs`     | B0    | Per-message allocation (Embassy buffer)  |
//! | `benches/b1_latency_embassy.rs`   | B1    | Push-to-recv latency (Embassy buffer)    |
//! | `benches/b2_throughput_embassy.rs`| B2    | Steady-state throughput (Embassy buffer) |
//! | `benches/b_alloc_pipeline.rs`     | info  | Per-message allocation (runner pipeline) |
//! | `benches/b_runner_pipeline.rs`    | info  | Runner pipeline throughput (Criterion)   |
//!
//! On-target cycle profiling (B3) is a separate hardware-only crate,
//! `examples/embassy-bench-stm32h5`, because DWT cycle counting cannot run on a
//! host. See design doc 038 for the on-target B3 harness.

pub mod alloc;
pub mod profiles;
pub mod profiles_embassy;
pub mod reports;
