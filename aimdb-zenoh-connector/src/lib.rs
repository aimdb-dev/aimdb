//! Zenoh connector for AimDB.
//!
//! Serves `zenoh://` links, and with `std` also `ros2://` links that follow
//! rmw_zenoh's conventions.
//!
//! ## Features
//!
//! - `std`: the `zenoh` crate backend, both schemes
//! - `embedded`: the `zenoh-nostd` backend over a caller-supplied transport,
//!   `zenoh://` only

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// Zenoh key expressions for inbound links (works on every feature leg).
pub mod grammar;
pub use grammar::ZenohGrammar;
