//! # AimDB Data Contracts
//!
//! Self-describing data schemas that work identically across MCU, edge, and cloud.
//!
//! This crate provides:
//! - **Schema types** - Data structures with unique identifiers
//! - **Contract profiles** - Configuration for runtime behavior
//! - **Simulation support** - Generate realistic test data
//!
//! ## Design Philosophy
//!
//! Contracts separate **what data looks like** (schema) from **how it behaves at runtime**
//! (policies). This enables:
//! - Reusable schemas across different deployment configurations
//! - Type-safe data exchange between systems
//! - Configurable policies without changing code
//!
//! ## Example
//!
//! ```rust
//! use aimdb_data_contracts::{SchemaType, Settable};
//! use aimdb_data_contracts::contracts::Temperature;
//!
//! // Create a reading
//! let temp = Temperature::set(22.5, 1704326400000);
//! assert_eq!(temp.celsius, 22.5);
//! assert_eq!(Temperature::NAME, "temperature");
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

pub mod contracts;

mod streamable;
pub use streamable::{is_streamable, Streamable};

#[cfg(feature = "linkable")]
mod linkable;

#[cfg(feature = "observable")]
mod observable;

#[cfg(feature = "observable")]
pub use observable::log_tap;

#[cfg(feature = "simulatable")]
mod simulatable;

#[cfg(feature = "migratable")]
mod migratable;

#[cfg(feature = "simulatable")]
pub use simulatable::{SimulationConfig, SimulationParams};

#[cfg(feature = "migratable")]
pub use migratable::{Migratable, MigrationError};

// ═══════════════════════════════════════════════════════════════════
// SCHEMA TRAITS (Implementation-defined)
// ═══════════════════════════════════════════════════════════════════

/// Identity and metadata for a data contract.
///
/// Every schema type has a unique name and version used for:
/// - Record registration in AimDB
/// - Profile matching in contract configurations
/// - Wire protocol identification
/// - Version compatibility checking
///
/// # Versioning and Backward Compatibility
///
/// The `VERSION` constant tracks schema evolution. When following backward
/// compatibility rules, a server running version N can safely ingest data
/// from producers running any version 1..=N.
///
/// ## Compatibility Rules
///
/// | Change Type | Allowed? | Example |
/// |-------------|----------|---------|
/// | Add optional field | ✅ Yes | `#[serde(default)]` new field |
/// | Add field with default | ✅ Yes | New field deserializes to default |
/// | Remove unused field | ✅ Yes | Old data with field still parses |
/// | Rename field | ⚠️ Migration | Use `Migratable` trait |
/// | Change field type | ⚠️ Migration | Use `Migratable` trait |
/// | Add required field | ⚠️ Migration | Use `Migratable` trait |
///
/// For breaking changes, implement the `Migratable` trait (requires `migration` feature)
/// to provide runtime transformation of older data formats.
pub trait SchemaType: Sized {
    /// Unique identifier for this schema (e.g., "temperature", "humidity")
    const NAME: &'static str;

    /// Schema version. Defaults to 1.
    ///
    /// Increment when adding new optional/defaulted fields.
    const VERSION: u32 = 1;
}

// ═══════════════════════════════════════════════════════════════════
// SIMULATABLE SUPPORT (feature = "simulatable")
// ═══════════════════════════════════════════════════════════════════

/// Generate realistic test/simulation data.
///
/// This is an intrinsic capability of the schema type itself,
/// not a policy decision. If a type can be simulated, implement this.
#[cfg(feature = "simulatable")]
pub trait Simulatable: SchemaType {
    /// Generate a new sample with optional reference to previous value.
    ///
    /// # Parameters
    /// - `config`: Simulation parameters (type-specific)
    /// - `previous`: Optional reference to last generated value (for random walks, trends)
    /// - `rng`: Random number generator
    /// - `timestamp`: Unix timestamp in milliseconds
    fn simulate<R: rand::Rng>(
        config: &SimulationConfig,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp: u64,
    ) -> Self;
}

/// Construct a schema instance from its primary value.
///
/// This defines the canonical way to create a new reading/measurement.
pub trait Settable: SchemaType {
    /// The primary value type (e.g., `f32` for temperature)
    type Value;

    /// Create a new instance from a value.
    ///
    /// # Parameters
    /// - `value`: The primary data value
    /// - `timestamp`: Unix timestamp in milliseconds
    fn set(value: Self::Value, timestamp: u64) -> Self;
}

// ═══════════════════════════════════════════════════════════════════
// OBSERVABLE SUPPORT
// ═══════════════════════════════════════════════════════════════════

/// Extract a signal value for observation.
///
/// Implement this trait to enable threshold checking, alerting,
/// and other signal-based operations on your schema type.
///
/// The extracted signal can be used by node implementations to:
/// - Check against configured thresholds
/// - Trigger alerts when bounds are exceeded
/// - Compute aggregations (mean, min, max)
/// - Feed into monitoring systems
/// - Format log output with `format_log()`
pub trait Observable: SchemaType {
    /// The numeric type of the signal (e.g., `f32`, `f64`, `i32`).
    ///
    /// Must be comparable and copyable for threshold checks.
    type Signal: PartialOrd + Copy;

    /// Icon/emoji for log output (e.g., "🌡️", "💧", "📊")
    ///
    /// Override this to provide a visual indicator for your data type.
    const ICON: &'static str = "📊";

    /// Unit label for the signal (e.g., "°C", "%", "hPa")
    ///
    /// Override this to display the appropriate unit in log output.
    const UNIT: &'static str = "";

    /// Extract the signal value from this instance.
    fn signal(&self) -> Self::Signal;

    /// Format a log entry for this observation.
    ///
    /// The default implementation uses `Debug` formatting. Override this
    /// for prettier, human-readable output.
    ///
    /// # Example output
    /// ```text
    /// 🌡️ [alpha] Temperature: 22.5°C at 1704326400000
    /// 💧 [beta] Humidity: 65.3% at 1704326400000
    /// ```
    fn format_log(&self, node_id: &str) -> alloc::string::String
    where
        Self: core::fmt::Debug,
    {
        alloc::format!("{} [{}] {:?}", Self::ICON, node_id, self)
    }
}

// ═══════════════════════════════════════════════════════════════════
// LINKABLE SUPPORT (feature = "linkable")
// ═══════════════════════════════════════════════════════════════════

/// Types that can be serialized/deserialized for connector links.
///
/// Implement this trait to enable `link_from` and `link_to` operations
/// in AimDB connectors (MQTT, KNX, etc.). This provides the wire format
/// for transporting schema types across network boundaries.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_data_contracts::{Linkable, contracts::Temperature};
///
/// // In connector configuration:
/// builder.configure::<Temperature>(NODE_ID, |reg| {
///     reg.buffer(BufferCfg::SingleLatest)
///         .link_from("mqtt://sensors/temperature")
///         .with_deserializer(Temperature::from_bytes)
///         .finish();
/// });
/// ```
#[cfg(feature = "linkable")]
pub trait Linkable: SchemaType + Sized {
    /// Deserialize from bytes (e.g., MQTT payload).
    ///
    /// Returns `Err` with error message on parse failure.
    fn from_bytes(data: &[u8]) -> Result<Self, String>;

    /// Serialize to bytes (e.g., for MQTT payload).
    ///
    /// Returns `Err` with error message on serialization failure.
    fn to_bytes(&self) -> Result<Vec<u8>, String>;
}
