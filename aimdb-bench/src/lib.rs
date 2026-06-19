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
//! | File                            | Class | Purpose                                  |
//! |---------------------------------|-------|------------------------------------------|
//! | `benches/b0_alloc_tokio.rs`     | B0    | Per-message allocation (buffer layer)    |
//! | `benches/b1_latency.rs`         | B1    | Push-to-recv latency (buffer layer)      |
//! | `benches/b2_throughput.rs`      | B2    | Steady-state throughput (buffer layer)   |
//! | `benches/b_alloc_pipeline.rs`   | info  | Per-message allocation (runner pipeline) |
//! | `benches/b_runner_pipeline.rs`  | info  | Runner pipeline throughput (Criterion)   |

pub mod alloc;
pub mod profiles;
pub mod reports;
