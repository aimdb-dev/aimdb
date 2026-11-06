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
//! Everything here is runtime-agnostic: the contracts and their wire format
//! ([`types`]), the record keys and their MQTT topics ([`keys`]), the
//! simulated thermometers as an input ([`sim`]), and the monitors
//! ([`monitors`]). A demo adds only its platform setup and the record wiring:
//!
//! ```rust,ignore
//! let mqtt = builder.connect(MqttConnector::new(broker_url));
//! let SimThermometers { indoor, .. } = SimThermometers::new();
//!
//! builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
//!         .input(Thermometer::on(indoor))
//!         .tap(temperature_logger)
//!         .output(publish(mqtt, SensorKey::TempIndoor.link_address().unwrap()));
//! });
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod keys;
pub mod monitors;
pub mod sim;
pub mod types;

// Re-export main types
pub use keys::{ButtonKey, CommandKey, SensorKey};
pub use monitors::{button_logger, command_consumer, temperature_logger};
pub use sim::{Reading, SimChannel, SimThermometers, Thermometer};
pub use types::{ButtonEvent, ButtonPress, CATALOG, Temperature, TemperatureCommand};
