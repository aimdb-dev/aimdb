//! Compile-time safe record keys for KNX connector demos.
//!
//! Using the `RecordKey` derive macro provides:
//! - **Type Safety**: Can't accidentally use wrong key for wrong record type
//! - **Refactoring**: Rename keys in one place, IDE helps find all usages
//! - **Discovery**: IDE autocomplete shows available keys
//! - **Documentation**: Enum variants can have doc comments
//! - **Connector Metadata**: KNX group addresses defined alongside keys
//!
//! # Example
//!
//! ```ignore
//! use knx_connector_demo_common::keys::{TemperatureKey, LightKey};
//!
//! // Type-safe key usage with auto-linked KNX address
//! builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
//!     // link_from() is automatically available from the key!
//!     ...
//! });
//! ```

use aimdb_core::RecordKey;

/// Keys for temperature sensor records (inbound: KNX → AimDB).
///
/// Each variant maps to a specific KNX group address for temperature readings.
#[derive(RecordKey, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[key_prefix = "temp."]
pub enum TemperatureKey {
    /// Living room temperature sensor (KNX: 9/1/0)
    #[key = "livingroom"]
    #[link_address = "knx://9/1/0"]
    LivingRoom,

    /// Bedroom temperature sensor (KNX: 9/0/1)
    #[key = "bedroom"]
    #[link_address = "knx://9/0/1"]
    Bedroom,

    /// Kitchen temperature sensor (KNX: 9/1/2)
    #[key = "kitchen"]
    #[link_address = "knx://9/1/2"]
    Kitchen,
}

/// Keys for light state monitor records (inbound: KNX → AimDB).
///
/// Each variant maps to a specific KNX group address for light status.
#[derive(RecordKey, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[key_prefix = "lights."]
pub enum LightKey {
    /// Main light monitor (KNX: 1/0/7)
    #[key = "main"]
    #[link_address = "knx://1/0/7"]
    Main,

    /// Hallway light monitor (KNX: 1/0/8)
    #[key = "hallway"]
    #[link_address = "knx://1/0/8"]
    Hallway,
}

/// Keys for light control records (outbound: AimDB → KNX).
///
/// Separate from `LightKey` to distinguish read (monitor) from write (control).
#[derive(RecordKey, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[key_prefix = "lights."]
pub enum LightControlKey {
    /// Light control output (KNX: 1/0/6)
    #[key = "control"]
    #[link_address = "knx://1/0/6"]
    Control,
}
