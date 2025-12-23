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

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod monitors;
pub mod types;

// Re-export main types
pub use monitors::{command_consumer, temperature_logger};
pub use types::{Temperature, TemperatureCommand};
