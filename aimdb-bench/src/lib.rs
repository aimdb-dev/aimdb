//! AimDB benchmarking infrastructure. **Not for production use.**
//!
//! Reusable primitives for B0 (allocation counting), B1 (latency), and B2
//! (throughput) benchmarks. `alloc::CountingAllocator` is declared as the
//! `#[global_allocator]` by each individual bench binary (and by the stm32h5
//! B3 example) — not by this crate — so nothing in the production dependency
//! graph is affected either way.
//!
//! `payloads` and `alloc` are `no_std`-clean so the `no_std`
//! `examples/embassy-bench-stm32h5` B3 rig can depend on this crate with
//! `default-features = false` and share them, instead of hand-forking its own
//! copies. `profiles`/`profiles_embassy`/`reports` — and the `criterion`/
//! `serde_json`/`std::fs` surface they pull in — require the `std` feature
//! (on by default; only ever built without it via `--no-default-features`
//! from the `no_std` example's Cargo.toml).
//!
//! # Bench entrypoints
//!
//! | File                              | Class | Purpose                                  |
//! |-----------------------------------|-------|------------------------------------------|
//! | `benches/b0_alloc_tokio.rs`       | B0    | Per-message allocation (Tokio buffer)    |
//! | `benches/b1_b2_tokio.rs`          | B1+B2 | Latency (time/iter) + throughput (Tokio) |
//! | `benches/b0_alloc_embassy.rs`     | B0    | Per-message allocation (Embassy buffer)  |
//! | `benches/b0_alloc_linkable.rs`    | B0    | Postcard `Linkable::encode_into` allocs  |
//! | `benches/b0_alloc_remote_ir.rs`   | B0    | JSON/CBOR remote-IR allocation comparison|
//! | `benches/b1_b2_embassy.rs`        | B1+B2 | Latency (time/iter) + throughput (Embassy)|
//! | `benches/b1_b2_remote_ir.rs`      | B1+B2 | JSON/CBOR remote-IR latency + throughput |
//! | `benches/b1_b2_remote_envelope.rs`| B1+B2 | Native consumer envelope prototype       |
//! | `benches/b_alloc_pipeline.rs`     | info  | Per-message allocation (runner pipeline) |
//! | `benches/b_runner_pipeline.rs`    | info  | Runner pipeline throughput (Criterion)   |
//! | `benches/b0_alloc_migration.rs`   | B0    | Per-call allocation (migrate_from_bytes) |
//!
//! On-target cycle profiling (B3) lives in the hardware-only
//! `examples/embassy-bench-stm32h5` crate, since DWT cycle counting cannot run
//! on a host.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod alloc;
#[cfg(feature = "std")]
pub mod harness;
pub mod payloads;
#[cfg(feature = "std")]
pub mod profiles;
pub mod profiles_embassy;
#[cfg(feature = "std")]
pub mod remote_envelope;
#[cfg(feature = "std")]
pub mod remote_ir;
#[cfg(feature = "std")]
pub mod reports;
