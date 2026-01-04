//! Standard data contracts

pub mod humidity;
pub mod location;
pub mod temperature;

pub use humidity::Humidity;
pub use location::GpsLocation;
pub use temperature::Temperature;
