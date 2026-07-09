//! # AimDB Data Contracts
//!
//! Trait definitions for self-describing data schemas that work identically
//! across MCU, edge, and cloud.
//!
//! This crate provides:
//! - **Schema types** — Data structures with unique identifiers ([`SchemaType`])
//! - **Streamable** — Marker for types that cross serialization boundaries ([`Streamable`])
//! - **Contract profiles** — Configuration for runtime behavior
//! - **Simulation support** — Generate realistic test data
//!
//! ## Design Philosophy
//!
//! Contracts separate **what data looks like** (schema) from **how it behaves at runtime**
//! (policies). This enables:
//! - Reusable schemas across different deployment configurations
//! - Type-safe data exchange between systems
//! - Configurable policies without changing code
//!
//! ## Defining a Custom Contract
//!
//! ```rust
//! use aimdb_data_contracts::{SchemaType, Streamable};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Debug, Serialize, Deserialize)]
//! pub struct MyCustomSensor {
//!     pub reading: f64,
//!     pub timestamp: u64,
//! }
//!
//! impl SchemaType for MyCustomSensor {
//!     const NAME: &'static str = "my_custom_sensor";
//! }
//!
//! // Mark as streamable — can cross WebSocket / WASM boundaries
//! impl Streamable for MyCustomSensor {}
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

// The `migration_chain!` proc-macro (aimdb-derive) has no `$crate` and so
// hardcodes absolute `::aimdb_data_contracts::...` paths in its expansion
// (matching the `RecordKey` -> `aimdb_core` precedent). That means this
// crate must be able to resolve itself by its own external name — which a
// crate cannot do by default (RFC 2126) — for the internal arity-check
// fixture in `migratable.rs` that invokes the macro from inside this same
// crate. External callers (doctests, integration tests, downstream
// consumers) don't need this; it only matters for that one self-reference.
#[cfg(feature = "migratable")]
extern crate self as aimdb_data_contracts;

/// Re-exports used by macro-generated code, kept out of the crate's own
/// namespace so a caller's own `serde_json`/`alloc` (if any) never shadows
/// or gets shadowed by them — see `migration_chain!`'s expansion.
#[doc(hidden)]
pub mod __private {
    pub extern crate alloc;
    #[cfg(any(feature = "linkable", feature = "migratable"))]
    pub use serde_json;
}

mod streamable;
pub use streamable::Streamable;

#[cfg(feature = "linkable")]
mod linkable;

#[cfg(feature = "linkable")]
pub use linkable::LinkableRegistrarExt;

/// `#[derive(Linkable)]` — see [`Linkable`] and `aimdb_derive::Linkable`'s docs.
///
/// Re-exported under the same name as the trait, following the trait+derive
/// pairing convention (`serde::Serialize`, `aimdb_derive::RecordKey`) — proc-macro
/// derive names live in a separate namespace from traits, so this is not a
/// conflict.
#[cfg(feature = "linkable")]
pub use aimdb_derive::Linkable;

#[cfg(feature = "observable")]
mod observable;

#[cfg(feature = "observable")]
pub use observable::{log_tap, ObservableRegistrarExt};

#[cfg(feature = "simulatable")]
mod simulatable;

#[cfg(feature = "migratable")]
mod migratable;

#[cfg(feature = "simulatable")]
pub use simulatable::{RandomWalkParams, SimProfile, Simulatable, SimulatableRegistrarExt};

#[cfg(feature = "migratable")]
pub use migratable::{MigrationChain, MigrationError, MigrationStep};

#[cfg(feature = "migratable")]
pub use aimdb_derive::migration_chain;

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
/// | Rename field | ⚠️ Migration | Use `MigrationStep` + `migration_chain!` |
/// | Change field type | ⚠️ Migration | Use `MigrationStep` + `migration_chain!` |
/// | Add required field | ⚠️ Migration | Use `MigrationStep` + `migration_chain!` |
///
/// For breaking changes, implement `MigrationStep` and use `migration_chain!` (requires `migratable` feature)
/// to provide runtime transformation of older data formats. `migratable` only requires `alloc` — schema
/// migration works identically on `no_std` targets (e.g. Embassy on an MCU) and on `std`.
pub trait SchemaType: Sized {
    /// Unique identifier for this schema (e.g., "temperature", "humidity")
    const NAME: &'static str;

    /// Schema version. Defaults to 1.
    ///
    /// Increment when adding new optional/defaulted fields.
    const VERSION: u32 = 1;
}

/// Construct a schema instance from its primary value.
///
/// This defines the canonical way to create a new reading/measurement — the
/// write counterpart to [`Observable::signal`]'s read projection. Implementing
/// it unlocks the `SyncProducer::set_value` family in `aimdb-sync`.
#[cfg(feature = "settable")]
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

/// Project a schema type onto a numeric domain signal.
///
/// The trait's kernel is the numeric projection: implement it, call
/// [`ObservableRegistrarExt::observe`],
/// and the signal is folded into live last/min/max/mean statistics that surface
/// on `record.list` / `record.get` and stage profiling. The signal can also feed
/// threshold checks, alerting, and aggregation.
///
/// Presentation is not part of the trait: `.log(node_id)` (also on the ext
/// trait) formats a human-readable line from `Debug` +
/// [`SIGNAL`](Observable::SIGNAL)/[`UNIT`](Observable::UNIT).
///
/// # Feature layering
///
/// Three layers, each useful without the next:
///
/// 1. **This trait** — always compiled, no features. [`signal()`](Observable::signal)
///    plus the `SIGNAL`/`UNIT` labels are enough for hand-wired `.tap()`s,
///    threshold checks, and generic code over `T: Observable`.
/// 2. **`observable` (this crate)** — unlocks the registrar verbs `.observe()`
///    and `.log()`. Gated only because the ext trait needs `alloc` and
///    `aimdb-core`.
/// 3. **`observability` (`aimdb-core`)** — the metrics backend. When it is off,
///    `.observe()` still compiles and runs but its gauge is inert: updates are
///    no-ops and nothing surfaces on `record.list` / `record.get` (see
///    `RecordRegistrar::signal_gauge`). `.log()` is unaffected. This lets
///    constrained targets ship `Observable` contracts while compiling the
///    metrics cost away.
pub trait Observable: SchemaType {
    /// The numeric type of the signal (e.g., `f32`, `f64`, `i32`).
    ///
    /// Must be comparable and copyable for threshold checks. `.observe()`
    /// additionally requires `Signal: Into<f64>` at its call site; a type with
    /// an exotic signal can still implement `Observable` and write its own tap.
    type Signal: PartialOrd + Copy;

    /// What the signal means, for metrics/UI labels (e.g. `"celsius"`).
    /// Defaults to the schema name.
    const SIGNAL: &'static str = Self::NAME;

    /// Unit label for the signal (e.g. `"°C"`, `"%"`, `"hPa"`).
    const UNIT: &'static str = "";

    /// Extract the signal value from this instance.
    fn signal(&self) -> Self::Signal;
}

// ═══════════════════════════════════════════════════════════════════
// LINKABLE SUPPORT (feature = "linkable")
// ═══════════════════════════════════════════════════════════════════

/// Types that can be serialized/deserialized for connector links.
///
/// Implement this trait, then call
/// [`LinkableRegistrarExt::linked_from`](linkable::LinkableRegistrarExt::linked_from) /
/// [`linked_to`](linkable::LinkableRegistrarExt::linked_to) to wire `link_from`
/// / `link_to` in AimDB connectors (MQTT, KNX, etc.) with the codec defaulted to
/// `from_bytes`/`to_bytes`. This provides the wire format for transporting
/// schema types across network boundaries.
///
/// # Example
///
/// Not compiled: the snippet needs `aimdb-core`'s builder types in scope.
///
/// ```rust,ignore
/// use aimdb_data_contracts::{Linkable, LinkableRegistrarExt};
/// use my_app::Temperature;  // user-defined type implementing Linkable
///
/// // In connector configuration:
/// builder.configure::<Temperature>(NODE_ID, |reg| {
///     reg.buffer(BufferCfg::SingleLatest);
///     reg.linked_from("mqtt://sensors/temperature");
/// });
/// ```
#[cfg(feature = "linkable")]
pub trait Linkable: SchemaType + Sized {
    /// Deserialize from bytes (e.g., MQTT payload).
    ///
    /// Returns `Err` with error message on parse failure.
    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String>;

    /// Serialize to bytes (e.g., for MQTT payload).
    ///
    /// Returns `Err` with error message on serialization failure.
    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String>;
}
