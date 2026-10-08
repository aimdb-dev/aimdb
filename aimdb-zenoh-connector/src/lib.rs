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

/// The URL scheme of plain Zenoh links.
pub(crate) const SCHEME: &str = "zenoh";

// One `ZenohConnector` over its protocol backends.
pub mod connector;
pub use connector::{Native, ZenohConnector};

// The `zenoh` crate backend.
#[cfg(feature = "std")]
mod native;

// Zenoh key expressions for inbound links (works on every feature leg).
pub mod grammar;
pub use grammar::ZenohGrammar;

// rmw_zenoh's wire conventions for `ros2://` links.
mod profile;
