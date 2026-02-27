//! Streamable trait for data contracts that can cross serialization boundaries.
//!
//! Types implementing [`Streamable`] can be transported across WebSocket, WASM,
//! and other wire boundaries with full contract enforcement (Rust serde
//! deserialization at the receiving end).
//!
//! # Design
//!
//! `Streamable` is a *capability marker* — it combines [`SchemaType`] identity
//! with the `serde` bounds needed for type-erased dispatch at serialization
//! boundaries. The companion [`dispatch_streamable!`] macro centralises the
//! schema-name → concrete-type routing so that consumers (WASM adapter,
//! WebSocket bridge, CLI) never hardcode contract types.
//!
//! # Adding a new streamable contract
//!
//! 1. Define your struct with `Serialize + Deserialize` in `contracts/`.
//! 2. Implement `SchemaType` (unique `NAME`).
//! 3. `impl Streamable for MyType {}` in this module.
//! 4. Add a match arm to [`dispatch_streamable!`].
//!
//! That's it — every consumer that uses the macro picks up the new type
//! automatically.

use crate::SchemaType;
use core::fmt::Debug;
use serde::{de::DeserializeOwned, Serialize};

/// Data contracts that can be transported across serialization boundaries.
///
/// Implementing this trait signals that a contract type supports:
/// - Type-erased JSON/`JsValue` serialization and deserialization
/// - Registration in AimDB builders by schema name string
/// - Cross-boundary dispatch (WASM bindings, WebSocket bridge, CLI)
///
/// # Bounds
///
/// The super-trait bounds mirror what AimDB's typed record APIs require:
/// `Send + Sync + Clone + Debug + 'static` plus serde `Serialize` and
/// `DeserializeOwned`. All standard data contracts satisfy these.
///
/// # Example
///
/// ```rust
/// use aimdb_data_contracts::{SchemaType, Streamable};
/// use aimdb_data_contracts::contracts::Temperature;
///
/// // Temperature implements Streamable — it can be used across boundaries
/// fn assert_streamable<T: Streamable>() {}
/// assert_streamable::<Temperature>();
/// ```
pub trait Streamable:
    SchemaType + Serialize + DeserializeOwned + Send + Sync + Clone + Debug + 'static
{
}

// ═══════════════════════════════════════════════════════════════════
// Implementations for built-in contracts
// ═══════════════════════════════════════════════════════════════════

use crate::contracts::{GpsLocation, Humidity, Temperature};

impl Streamable for Temperature {}
impl Streamable for Humidity {}
impl Streamable for GpsLocation {}

/// Returns `true` if `name` matches a known [`Streamable`] contract's
/// [`SchemaType::NAME`].
///
/// Useful for early validation before dispatch.
pub fn is_streamable(name: &str) -> bool {
    matches!(
        name,
        <Temperature as SchemaType>::NAME
            | <Humidity as SchemaType>::NAME
            | <GpsLocation as SchemaType>::NAME
    )
}

/// Dispatch a schema type name to a typed code block.
///
/// Routes a runtime `&str` schema name to the concrete Rust type that
/// implements [`Streamable`], then executes `$body` with `$T` bound to
/// that type. Returns `Some(body_result)` on match, `None` if the schema
/// name is unknown.
///
/// This is the **single source of truth** for the schema-name → type
/// mapping — WASM bindings, WebSocket bridge, and other consumers all
/// use this macro instead of maintaining their own tables.
///
/// # Usage
///
/// ```rust,ignore
/// use aimdb_data_contracts::dispatch_streamable;
///
/// let result = dispatch_streamable!(schema_name, |T| {
///     // `T` is the concrete type (Temperature, Humidity, GpsLocation, …)
///     builder.configure::<T>(key, |reg| reg.buffer(cfg));
/// })
/// .ok_or_else(|| format!("Unknown schema: {schema_name}"))?;
/// ```
///
/// # Adding a new contract
///
/// 1. `impl Streamable for NewType {}` (above)
/// 2. Add a match arm here.
#[macro_export]
macro_rules! dispatch_streamable {
    ($schema_name:expr, |$T:ident| $body:expr) => {
        match $schema_name {
            <$crate::contracts::Temperature as $crate::SchemaType>::NAME => {
                type $T = $crate::contracts::Temperature;
                Some($body)
            }
            <$crate::contracts::Humidity as $crate::SchemaType>::NAME => {
                type $T = $crate::contracts::Humidity;
                Some($body)
            }
            <$crate::contracts::GpsLocation as $crate::SchemaType>::NAME => {
                type $T = $crate::contracts::GpsLocation;
                Some($body)
            }
            _ => None,
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_schemas_are_streamable() {
        assert!(is_streamable("temperature"));
        assert!(is_streamable("humidity"));
        assert!(is_streamable("gps_location"));
    }

    #[test]
    fn unknown_schema_is_not_streamable() {
        assert!(!is_streamable("unknown"));
        assert!(!is_streamable(""));
    }

    #[test]
    fn dispatch_routes_correctly() {
        let result = dispatch_streamable!("temperature", |T| <T as SchemaType>::NAME);
        assert_eq!(result.unwrap(), "temperature");

        let result = dispatch_streamable!("humidity", |T| <T as SchemaType>::NAME);
        assert_eq!(result.unwrap(), "humidity");

        let result = dispatch_streamable!("gps_location", |T| <T as SchemaType>::NAME);
        assert_eq!(result.unwrap(), "gps_location");
    }

    #[test]
    fn dispatch_rejects_unknown() {
        let result = dispatch_streamable!("unknown", |_T| ());
        assert!(result.is_none());
    }
}
