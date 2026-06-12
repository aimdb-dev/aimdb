//! KNX/IP connector for AimDB
//!
//! Provides bidirectional KNX integration for AimDB records:
//! - **Outbound**: Automatic publishing from AimDB to KNX group addresses
//! - **Inbound**: Monitor KNX bus and produce into AimDB buffers
//!
//! ## Features
//!
//! - `tokio-runtime`: Tokio-based connector using UDP sockets
//! - `embassy-runtime`: Embassy connector for embedded systems
//! - `tracing`: Debug logging support (std)
//! - `defmt`: Debug logging support (no_std)
//!
//! ## Production Status
//!
//! **Current Version: 0.1.0 - Beta Quality**
//!
//! ✅ **Ready for production use with caveats:**
//! - Core protocol implementation is stable
//! - ACK handling and timeout detection implemented
//! - Automatic reconnection on failures
//! - Comprehensive unit tests
//!
//! ⚠️ **Known limitations:**
//! - No KNX Secure support (plaintext only)
//! - No group address discovery
//! - Limited DPT helpers (use `knx-pico` crate)
//! - Fire-and-forget publish (no bus-level confirmation)
//!
//! See README.md for full deployment guide.
//!
//! ## Tokio Usage (Standard Library)
//!
//! ```no_run
//! use aimdb_core::buffer::BufferCfg;
//! use aimdb_core::AimDbBuilder;
//! use aimdb_knx_connector::KnxConnector;
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone)]
//! struct LightState {
//!     is_on: bool,
//! }
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let runtime = Arc::new(TokioAdapter::new()?);
//!
//! let mut builder = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(KnxConnector::new("knx://192.168.1.19:3671"));
//! builder.configure::<LightState>("light.state", |reg| {
//!     reg.buffer(BufferCfg::SingleLatest)
//!        // Inbound: Monitor KNX bus
//!        .link_from("knx://1/0/7")
//!        .with_deserializer_raw(|data: &[u8]| {
//!            let is_on = data.first().map(|&b| b != 0).unwrap_or(false);
//!            Ok(LightState { is_on })
//!        })
//!        .finish()
//!        // Outbound: Send commands to KNX
//!        .link_to("knx://1/0/8")
//!        .with_serializer_raw(|state: &LightState| {
//!            Ok(vec![if state.is_on { 1 } else { 0 }])
//!        })
//!        .finish();
//! });
//! let (db, runner) = builder.build().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Embassy Usage (Embedded)
//!
//! Illustrative (not compiled: requires the `embassy-runtime` feature and a
//! device network stack):
//!
//! ```rust,ignore
//! use aimdb_core::AimDbBuilder;
//! use aimdb_embassy_adapter::EmbassyAdapter;
//! use aimdb_knx_connector::embassy_client::KnxConnectorBuilder;
//! use alloc::sync::Arc;
//!
//! let runtime = Arc::new(EmbassyAdapter::new());
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(KnxConnectorBuilder::new("knx://192.168.1.19:3671", stack))
//!     .configure::<SensorData>(|reg| {
//!         reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
//!            .source(sensor_producer)
//!            // Inbound: Monitor KNX bus
//!            .link_from("knx://1/0/10")
//!            .with_deserializer_raw(|data| SensorData::from_knx(data))
//!            .finish()
//!            // Outbound: Send to KNX
//!            .link_to("knx://1/0/11")
//!            .with_serializer_raw(|data| data.to_knx_bytes())
//!            .finish();
//!     })
//!     .build().await?;
//! ```
//!
//! ## Group Address Format
//!
//! Group addresses use 3-level notation: `main/middle/sub`
//! - Main: 0-31 (5 bits)
//! - Middle: 0-7 (3 bits)
//! - Sub: 0-255 (8 bits)
//!
//! Example: `knx://192.168.1.19:3671/1/0/7`
//!
//! ## DPT Support
//!
//! This connector uses `knx-pico` for Data Point Type conversion:

#![cfg_attr(not(feature = "std"), no_std)]

// The shared tunnel engine uses `alloc` types on both runtimes.
extern crate alloc;

// Re-export knx-pico types for user convenience
pub use knx_pico::GroupAddress;

// Re-export DPT module for encoding/decoding KNX data types
/// KNX Datapoint Types (DPT) for encoding and decoding telegrams
///
/// ```rust
/// use aimdb_knx_connector::dpt::{Dpt1, Dpt9, DptEncode, DptDecode};
///
/// let mut buf = [0u8; 4];
///
/// // Boolean (DPT 1.001)
/// let len = Dpt1::Switch.encode(true, &mut buf)?;
///
/// // Temperature (DPT 9.001)
/// let len = Dpt9::Temperature.encode(21.5, &mut buf)?;
/// let temp = Dpt9::Temperature.decode(&buf[..len])?;
/// # Ok::<(), knx_pico::error::KnxError>(())
/// ```
pub mod dpt {
    pub use knx_pico::dpt::*;
}

// Convenience re-exports for common types (std only for backward compat)
#[cfg(feature = "std")]
pub use knx_pico::dpt::{Dpt1, Dpt5, Dpt9, DptDecode, DptEncode};

// Runtime-neutral KNX/IP tunneling state machine shared by both transports.
pub mod tunnel;

// Platform-specific implementations
#[cfg(feature = "tokio-runtime")]
pub mod tokio_client;

#[cfg(feature = "embassy-runtime")]
pub mod embassy_client;

// Re-export platform-specific types
// Both implementations use KnxConnectorBuilder for API consistency
// When both features are enabled (e.g., during testing), prefer tokio
#[cfg(all(feature = "tokio-runtime", not(feature = "embassy-runtime")))]
pub use tokio_client::KnxConnectorBuilder as KnxConnector;

#[cfg(all(feature = "embassy-runtime", not(feature = "tokio-runtime")))]
pub use embassy_client::KnxConnectorBuilder as KnxConnector;

// When both features are enabled, export both with different names
#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::KnxConnectorBuilder as TokioKnxConnector;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use embassy_client::KnxConnectorBuilder as EmbassyKnxConnector;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::KnxConnectorBuilder as KnxConnector; // Default to tokio when both enabled
