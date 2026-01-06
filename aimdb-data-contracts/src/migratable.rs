//! Runtime schema migration support.
//!
//! This module provides the `Migratable` trait for handling breaking schema
//! changes at runtime through JSON transformation.

use crate::SchemaType;

/// Error returned when schema migration fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    /// The source version is newer than this schema supports
    VersionTooNew { source: u32, current: u32 },
    /// A required field is missing and has no default
    MissingField(&'static str),
    /// Type conversion failed
    TypeConversion {
        field: &'static str,
        expected: &'static str,
    },
    /// Custom migration error
    Custom(&'static str),
}

impl core::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::VersionTooNew { source, current } => {
                write!(
                    f,
                    "source version {} is newer than current {}",
                    source, current
                )
            }
            Self::MissingField(field) => write!(f, "missing required field: {}", field),
            Self::TypeConversion { field, expected } => {
                write!(
                    f,
                    "type conversion failed for '{}', expected {}",
                    field, expected
                )
            }
            Self::Custom(msg) => write!(f, "{}", msg),
        }
    }
}

/// Runtime schema migration support.
///
/// Implement this trait to handle breaking schema changes at runtime.
/// The `migrate` function transforms raw JSON data from older versions
/// to the current schema format before deserialization.
///
/// # When to Use
///
/// Use `Migratable` when you need to:
/// - Rename fields while maintaining backward compatibility
/// - Change field types (e.g., int to float)
/// - Add required fields with computed defaults
/// - Handle complex structural changes
///
/// For simple additive changes (new optional fields), just use
/// `#[serde(default)]` - no migration needed.
///
/// # Example
///
/// ```rust
/// use aimdb_data_contracts::{SchemaType, Migratable, MigrationError};
/// use serde::{Deserialize, Serialize};
/// use serde_json::Value;
///
/// #[derive(Serialize, Deserialize)]
/// struct Temperature {
///     celsius: f32,      // Was "temp" in v1
///     timestamp: u64,
///     unit: String,      // Added in v3 as required field
/// }
///
/// impl SchemaType for Temperature {
///     const NAME: &'static str = "temperature";
///     const VERSION: u32 = 3;
/// }
///
/// impl Migratable for Temperature {
///     fn migrate(raw: &mut Value, from_version: u32) -> Result<(), MigrationError> {
///         // v1 -> v2: "temp" was renamed to "celsius"
///         if from_version < 2 {
///             if let Some(v) = raw.get("temp").cloned() {
///                 raw["celsius"] = v;
///                 raw.as_object_mut().unwrap().remove("temp");
///             }
///         }
///
///         // v2 -> v3: added required "unit" field
///         if from_version < 3 {
///             if raw.get("unit").is_none() {
///                 raw["unit"] = Value::String("celsius".into());
///             }
///         }
///
///         Ok(())
///     }
/// }
/// ```
pub trait Migratable: SchemaType {
    /// Migrate raw JSON data from an older version to the current schema.
    ///
    /// Called during deserialization when `from_version < VERSION`.
    /// Mutate `raw` in place to transform it to the current schema format.
    ///
    /// # Parameters
    /// - `raw`: Mutable reference to the JSON value to transform
    /// - `from_version`: The version of the incoming data
    ///
    /// # Returns
    /// - `Ok(())` if migration succeeded
    /// - `Err(MigrationError)` if migration failed
    fn migrate(raw: &mut serde_json::Value, from_version: u32) -> Result<(), MigrationError>;

    /// Deserialize with automatic migration from older versions.
    ///
    /// This is a convenience method that handles version checking and migration.
    fn deserialize_versioned(
        raw: &mut serde_json::Value,
        from_version: u32,
    ) -> Result<Self, MigrationError>
    where
        Self: serde::de::DeserializeOwned,
    {
        if from_version > Self::VERSION {
            return Err(MigrationError::VersionTooNew {
                source: from_version,
                current: Self::VERSION,
            });
        }

        if from_version < Self::VERSION {
            Self::migrate(raw, from_version)?;
        }

        serde_json::from_value(raw.clone())
            .map_err(|_| MigrationError::Custom("deserialization failed after migration"))
    }
}
