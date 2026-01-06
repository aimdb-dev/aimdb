//! # Weather Mesh Common
//!
//! Shared contracts, configuration, and types for the weather mesh demo.
//!
//! This crate is `no_std` compatible for use on MCU nodes.

#![cfg_attr(not(feature = "std"), no_std)]

// Re-export contracts used in the mesh
pub use aimdb_data_contracts::contracts::{GpsLocation, Humidity, Temperature};
pub use aimdb_data_contracts::{SchemaType, Settable};

// Re-export RecordKey for convenience
pub use aimdb_core::RecordKey;

/// Temperature record keys for each weather station node.
///
/// Each variant represents a temperature sensor with its MQTT topic.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[key_prefix = "temp."]
pub enum TempKey {
    #[key = "alpha"]
    #[link_address = "mqtt://sensors/alpha/temperature"]
    Alpha,

    #[key = "beta"]
    #[link_address = "mqtt://sensors/beta/temperature"]
    Beta,

    #[key = "gamma"]
    #[link_address = "mqtt://sensors/gamma/temperature"]
    Gamma,
}

/// Humidity record keys for each weather station node.
///
/// Each variant represents a humidity sensor with its MQTT topic.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[key_prefix = "humidity."]
pub enum HumidityKey {
    #[key = "alpha"]
    #[link_address = "mqtt://sensors/alpha/humidity"]
    Alpha,

    #[key = "beta"]
    #[link_address = "mqtt://sensors/beta/humidity"]
    Beta,

    #[key = "gamma"]
    #[link_address = "mqtt://sensors/gamma/humidity"]
    Gamma,
}
