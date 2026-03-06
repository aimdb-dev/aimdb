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
//! boundaries. The companion [`for_each_streamable`] function is the single
//! source of truth for which types are streamable — consumers implement
//! [`StreamableVisitor`] to build whatever dispatch tables they need.
//!
//! # Adding a new streamable contract
//!
//! 1. Define your struct with `Serialize + Deserialize` in `contracts/`.
//! 2. Implement `SchemaType` (unique `NAME`).
//! 3. `impl Streamable for MyType {}` below.
//! 4. Add `visitor.visit::<MyType>();` in [`for_each_streamable`].
//!
//! That's it — every consumer that uses the visitor picks up the new type
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

/// Visitor trait for iterating over all registered [`Streamable`] types.
///
/// Implement this trait to build type-erased dispatch tables, registries,
/// or any other structure that needs to know about all streamable types.
///
/// # Example
///
/// ```rust
/// use std::any::TypeId;
/// use aimdb_data_contracts::{SchemaType, Streamable, StreamableVisitor, for_each_streamable};
///
/// struct TypeIdCollector {
///     entries: Vec<(TypeId, &'static str)>,
/// }
///
/// impl StreamableVisitor for TypeIdCollector {
///     fn visit<T: Streamable>(&mut self) {
///         self.entries.push((TypeId::of::<T>(), T::NAME));
///     }
/// }
///
/// let mut collector = TypeIdCollector { entries: Vec::new() };
/// for_each_streamable(&mut collector);
/// assert_eq!(collector.entries.len(), 3);
/// ```
pub trait StreamableVisitor {
    /// Called once for each registered [`Streamable`] type.
    fn visit<T: Streamable>(&mut self);
}

// ═══════════════════════════════════════════════════════════════════
// Implementations for built-in contracts
// ═══════════════════════════════════════════════════════════════════

use crate::contracts::{GpsLocation, Humidity, Temperature};

impl Streamable for Temperature {}
impl Streamable for Humidity {}
impl Streamable for GpsLocation {}

/// Iterate over every registered [`Streamable`] type via the visitor pattern.
///
/// This is the **single source of truth** for which types are streamable.
/// All consumers (WASM adapter, WebSocket connector, CLI) use this function
/// to discover streamable types instead of maintaining their own lists.
///
/// # Adding a new contract
///
/// 1. `impl Streamable for NewType {}` (above)
/// 2. Add `visitor.visit::<NewType>();` here.
pub fn for_each_streamable(visitor: &mut impl StreamableVisitor) {
    visitor.visit::<Temperature>();
    visitor.visit::<Humidity>();
    visitor.visit::<GpsLocation>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::any::TypeId;

    struct NameCollector {
        names: alloc::vec::Vec<&'static str>,
    }

    impl StreamableVisitor for NameCollector {
        fn visit<T: Streamable>(&mut self) {
            self.names.push(T::NAME);
        }
    }

    struct TypeIdResolver {
        target: TypeId,
        result: Option<&'static str>,
    }

    impl StreamableVisitor for TypeIdResolver {
        fn visit<T: Streamable>(&mut self) {
            if TypeId::of::<T>() == self.target {
                self.result = Some(T::NAME);
            }
        }
    }

    #[test]
    fn visitor_discovers_all_types() {
        let mut c = NameCollector {
            names: alloc::vec::Vec::new(),
        };
        for_each_streamable(&mut c);
        assert!(c.names.contains(&"temperature"));
        assert!(c.names.contains(&"humidity"));
        assert!(c.names.contains(&"gps_location"));
        assert_eq!(c.names.len(), 3);
    }

    #[test]
    fn visitor_resolves_type_id() {
        let mut r = TypeIdResolver {
            target: TypeId::of::<Temperature>(),
            result: None,
        };
        for_each_streamable(&mut r);
        assert_eq!(r.result, Some("temperature"));
    }

    #[test]
    fn visitor_returns_none_for_unknown() {
        let mut r = TypeIdResolver {
            target: TypeId::of::<u32>(),
            result: None,
        };
        for_each_streamable(&mut r);
        assert_eq!(r.result, None);
    }

    #[test]
    fn known_schemas_are_discoverable() {
        let mut c = NameCollector {
            names: alloc::vec::Vec::new(),
        };
        for_each_streamable(&mut c);
        assert!(c.names.contains(&"temperature"));
        assert!(c.names.contains(&"humidity"));
        assert!(c.names.contains(&"gps_location"));
        assert!(!c.names.contains(&"unknown"));
    }
}
