//! KNX Demo Common Library
//!
//! Shared business logic for KNX connector demos that runs identically on:
//! - **MCU**: Embassy on STM32, RP2040, etc.
//! - **Edge**: Tokio on Linux/Raspberry Pi
//! - **Cloud**: Tokio on servers/containers
//!
//! # Design Philosophy
//!
//! This crate demonstrates AimDB's core strength: **write once, run anywhere**.
//!
//! The same monitor functions, data types, and business logic work across all
//! platforms by being generic over the `RuntimeAdapter` trait. Only the
//! platform-specific initialization (hardware setup, network stack) differs.
//!
//! # Example Usage
//!
//! ```ignore
//! use knx_demo_common::{TemperatureReading, LightState, temperature_monitor, light_monitor};
//!
//! // Same code works with TokioAdapter or EmbassyAdapter
//! builder.configure::<TemperatureReading>("temp.livingroom", |reg| {
//!     reg.buffer(...)
//!        .tap(temperature_monitor("Living Room", "🏠"))
//!        .link_from("knx://9/1/0")
//!        .with_deserializer(TemperatureReading::from_dpt9)
//!        .finish();
//! });
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod types;
pub mod monitors;

// Re-export main types
pub use types::{TemperatureReading, LightState, LightControl};
pub use monitors::{temperature_monitor, light_monitor};
