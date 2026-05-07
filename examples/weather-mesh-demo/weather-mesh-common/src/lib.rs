//! # Weather Mesh Common
//!
//! Shared contracts, configuration, and types for the weather mesh demo.
//!
//! This crate is `no_std` compatible for use on MCU nodes.

#![cfg_attr(not(feature = "std"), no_std)]

// Local contract definitions (Temperature, Humidity, GpsLocation, DewPoint)
pub mod contracts;
pub use contracts::{DewPoint, GpsLocation, Humidity, Temperature};

// Re-export traits from aimdb-data-contracts
pub use aimdb_data_contracts::{SchemaType, Settable, Streamable};

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

/// Dew point record keys for each weather station node.
///
/// Dew point is derived from the corresponding [`TempKey`] and [`HumidityKey`]
/// via `transform_join` — not sensed directly.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[key_prefix = "dew_point."]
pub enum DewPointKey {
    #[key = "alpha"]
    #[link_address = "mqtt://sensors/alpha/dew_point"]
    Alpha,

    #[key = "beta"]
    #[link_address = "mqtt://sensors/beta/dew_point"]
    Beta,

    #[key = "gamma"]
    #[link_address = "mqtt://sensors/gamma/dew_point"]
    Gamma,
}
