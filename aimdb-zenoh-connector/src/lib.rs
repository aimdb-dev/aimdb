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
//! - `transport-tls`, `transport-quic`, `transport-ws`: further Zenoh
//!   transports for the `std` backend
//!
//! ## Zenoh transport compression
//!
//! This crate never enables `zenoh`'s `transport_compression`. With it,
//! zenoh 1.10.1 decompresses through `lz4_flex` 0.10.0, which has
//! RUSTSEC-2026-0041 (eclipse-zenoh/zenoh#2589). An application whose own
//! dependencies enable it, for example `zenoh` with default features,
//! compiles that path in.

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

// `ros2://` links, on the `zenoh` crate backend.
#[cfg(feature = "std")]
mod ros2;
#[cfg(feature = "std")]
pub use ros2::{Reliability, Ros2Connector, Ros2Node};

// ROS 2 QoS overrides over core's link builders.
#[cfg(feature = "std")]
mod link_ext;
#[cfg(feature = "std")]
pub use link_ext::Ros2LinkExt;

// Zenoh key expressions for inbound links (works on every feature leg).
pub mod grammar;
pub use grammar::ZenohGrammar;

// rmw_zenoh's wire conventions for `ros2://` links.
mod profile;
