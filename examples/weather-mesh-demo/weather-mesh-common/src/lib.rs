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

/// Node identifiers for the weather mesh network.
///
/// Each variant represents a sensor station with its base MQTT topic.
/// The data type suffix (temperature, humidity) is added during buffer configuration.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[key_prefix = "node."]
pub enum NodeKey {
    #[key = "alpha"]
    #[link_address = "mqtt://sensors/alpha/"]
    Alpha,

    #[key = "beta"]
    #[link_address = "mqtt://sensors/beta/"]
    Beta,

    #[key = "gamma"]
    #[link_address = "mqtt://sensors/gamma/"]
    Gamma,
}
