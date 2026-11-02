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
//! Everything here is runtime-agnostic: the records ([`types`]), the record
//! keys and their group addresses ([`keys`]), the DPT (de)serializers
//! ([`dpt`]) and the monitors ([`monitors`]). A demo adds only its platform
//! setup, its inputs and the record wiring:
//!
//! ```rust,ignore
//! let knx = builder.connect(KnxConnector::new(..));
//!
//! builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
//!     reg.buffer(BufferCfg::SingleLatest)
//!         .tap(temperature_monitor)
//!         .input(
//!             InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
//!                 .with_deserializer(dpt::temperature_at("Living Room")),
//!         );
//! });
//! ```
//!
//! A toggle input (a key, a push button) yields [`ButtonEvent`]s;
//! [`LightControl`] is built from them with `From`.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod dpt;
pub mod keys;
pub mod monitors;
pub mod types;

// Re-export main types
pub use keys::{LightControlKey, LightKey, TemperatureKey};
pub use monitors::{light_monitor, temperature_monitor};
pub use types::{ButtonEvent, LightControl, LightState, TemperatureReading};
