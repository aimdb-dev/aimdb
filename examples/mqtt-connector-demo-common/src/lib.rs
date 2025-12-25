//! MQTT Demo Common Library
//!
//! Shared business logic for MQTT connector demos that runs identically on:
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
//! # Compile-Time Safe Keys
//!
//! This crate also demonstrates the `RecordKey` derive macro for type-safe
//! record keys. See the [`keys`] module for examples.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "derive")]
pub mod keys;
pub mod monitors;
pub mod types;

// Re-export main types
#[cfg(feature = "derive")]
pub use keys::{CommandKey, SensorKey};
pub use monitors::{command_consumer, temperature_logger};
pub use types::{Temperature, TemperatureCommand};
