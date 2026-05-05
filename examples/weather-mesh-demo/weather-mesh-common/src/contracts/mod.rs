//! Weather demo data contracts.
//!
//! These are example implementations of AimDB data contracts for weather
//! monitoring. They demonstrate how to implement `SchemaType`, `Streamable`,
//! `Observable`, `Settable`, `Linkable`, `Simulatable`, and `Migratable`
//! traits from `aimdb-data-contracts`.

pub mod dew_point;
pub mod humidity;
pub mod location;
pub mod temperature;

pub use dew_point::DewPoint;
pub use humidity::Humidity;
pub use location::GpsLocation;
pub use temperature::{Temperature, TemperatureV1, TemperatureV2};
