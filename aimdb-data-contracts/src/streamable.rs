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
//! boundaries. Users register their `Streamable` types with connectors and
//! adapters via `.register::<T>()` builder methods.
//!
//! # Implementing Streamable for a Custom Type
//!
//! 1. Define your struct with `Serialize + Deserialize`.
//! 2. Implement [`SchemaType`] (unique `NAME`).
//! 3. `impl Streamable for MyType {}`.
//! 4. Register it with your connector: `.register::<MyType>()`.
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
//! impl Streamable for MyCustomSensor {}
//! ```

use crate::SchemaType;
use core::fmt::Debug;
use serde::{de::DeserializeOwned, Serialize};

/// Data contracts that can be transported across serialization boundaries.
///
/// Implementing this trait signals that a contract type supports:
/// - Type-erased JSON/`JsValue` serialization and deserialization
/// - Registration in AimDB builders by schema name string
/// - Cross-boundary dispatch (WASM bindings, WebSocket bridge)
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
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Clone, Debug, Serialize, Deserialize)]
/// pub struct Pressure {
///     pub hpa: f32,
///     pub timestamp: u64,
/// }
///
/// impl SchemaType for Pressure {
///     const NAME: &'static str = "pressure";
/// }
///
/// impl Streamable for Pressure {}
///
/// fn assert_streamable<T: Streamable>() {}
/// assert_streamable::<Pressure>();
/// ```
pub trait Streamable:
    SchemaType + Serialize + DeserializeOwned + Send + Sync + Clone + Debug + 'static
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct TestSensor {
        value: f32,
        timestamp: u64,
    }

    impl SchemaType for TestSensor {
        const NAME: &'static str = "test_sensor";
    }

    impl Streamable for TestSensor {}

    #[test]
    fn streamable_has_schema_name() {
        assert_eq!(TestSensor::NAME, "test_sensor");
    }

    #[test]
    fn streamable_requires_schema_type() {
        fn assert_streamable<T: Streamable>() {}
        assert_streamable::<TestSensor>();
    }

    #[test]
    fn streamable_requires_serde() {
        let sensor = TestSensor {
            value: 42.5,
            timestamp: 1000,
        };

        // Serialize
        let json = serde_json::to_string(&sensor).unwrap();
        assert!(json.contains("42.5"));

        // Deserialize
        let restored: TestSensor = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.value, 42.5);
    }
}
