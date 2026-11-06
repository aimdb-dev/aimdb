//! Build-only fixture for the embedded checks.
//!
//! It stands in for an Embassy application's contracts and wiring, so that CI
//! compiles, on `thumbv7em-none-eabihf`, what an MCU application writes:
//!
//! - contracts that are `Linkable` (JSON), one of them through a user-written
//!   migration chain, all locked in `aimdb.lock`;
//! - two sourced records and a `transform_join` over them on the Embassy
//!   adapter, each linked out through its contract's codec;
//! - with `sim`, the same records produced by `.simulate()`.
//!
//! The wiring is built for `target_os = "none"` only; on the host, where
//! `cargo test` runs the lock check, the crate is just its contracts.
//!
//! `make check-no-sim` uses it to prove that the default graph has no `rand`
//! and the `sim` graph has. `cargo test` checks the contracts against
//! `aimdb.lock`. The wiring is compiled, never run.

#![no_std]

extern crate alloc;

pub mod contracts;

pub use contracts::{DewPoint, Humidity, Temperature};

#[cfg(target_os = "none")]
mod wiring;
#[cfg(target_os = "none")]
pub use wiring::*;
