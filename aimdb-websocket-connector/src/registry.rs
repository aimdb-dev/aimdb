//! Type-erased dispatch registry for [`Streamable`] types.
//!
//! Built incrementally via [`StreamableRegistry::register::<T>()`] at
//! connector construction time. Each entry stores monomorphized closures
//! that capture the concrete type `T` — no `Box<dyn Any>` or runtime
//! downcast needed.

use std::any::TypeId;
use std::collections::HashMap;

use aimdb_data_contracts::Streamable;

// ─── Type-erased operations ───────────────────────────────────────

/// Type-erased serialization closure: takes `&dyn Any`, downcasts to `T`,
/// and serializes to JSON bytes.
type SerializeFn = Box<dyn Fn(&dyn std::any::Any) -> Result<Vec<u8>, String> + Send + Sync>;

/// Type-erased deserialization closure: takes JSON bytes and produces a
/// boxed `Any` value of the concrete type `T`.
type DeserializeFn =
    Box<dyn Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send + Sync>, String> + Send + Sync>;

/// Type-erased operations for a single [`Streamable`] type.
///
/// Each field is a monomorphized closure that captures `T` at compile time
/// through generic instantiation. The concrete type is baked in — no
/// enum dispatch or trait object downcast is needed on the hot path.
#[allow(dead_code)]
pub(crate) struct StreamableOps {
    /// The `TypeId` of the concrete type.
    pub type_id: TypeId,
    /// The schema name (`T::NAME`).
    pub name: &'static str,
    /// Serialize a `&dyn Any` (known to be `&T`) to JSON bytes.
    pub serialize: SerializeFn,
    /// Deserialize JSON bytes into a `Box<dyn Any>` (actually `Box<T>`).
    pub deserialize: DeserializeFn,
}

// ─── Registry ─────────────────────────────────────────────────────

/// Maps schema names and type IDs to type-erased operations.
///
/// Built incrementally via [`register::<T>()`](StreamableRegistry::register)
/// before the connector is started.
pub(crate) struct StreamableRegistry {
    /// Schema name → operations.
    pub name_to_ops: HashMap<&'static str, StreamableOps>,
    /// TypeId → schema name (for outbound topic resolution).
    pub type_id_to_name: HashMap<TypeId, &'static str>,
}

impl StreamableRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            name_to_ops: HashMap::new(),
            type_id_to_name: HashMap::new(),
        }
    }

    /// Register a [`Streamable`] type.
    ///
    /// Each call monomorphizes closures for `T`'s serialization and
    /// deserialization. Duplicate registrations for the same type are
    /// idempotent (last wins).
    pub fn register<T: Streamable>(&mut self) {
        let type_id = TypeId::of::<T>();
        let name = T::NAME;

        let ops = StreamableOps {
            type_id,
            name,
            serialize: Box::new(|any_ref| {
                let value = any_ref
                    .downcast_ref::<T>()
                    .expect("type mismatch: registry is internally consistent");
                serde_json::to_vec(value).map_err(|e| e.to_string())
            }),
            deserialize: Box::new(|bytes| {
                let value: T = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                Ok(Box::new(value))
            }),
        };

        self.name_to_ops.insert(name, ops);
        self.type_id_to_name.insert(type_id, name);
    }

    /// Look up operations by schema name.
    #[allow(dead_code)]
    pub fn get_by_name(&self, name: &str) -> Option<&StreamableOps> {
        self.name_to_ops.get(name)
    }

    /// Resolve a `TypeId` to its schema name.
    #[allow(dead_code)]
    pub fn resolve_name(&self, type_id: &TypeId) -> Option<&'static str> {
        self.type_id_to_name.get(type_id).copied()
    }

    /// Returns all registered schema names.
    #[allow(dead_code)]
    pub fn known_names(&self) -> Vec<&'static str> {
        self.name_to_ops.keys().copied().collect()
    }

    /// Returns `true` if no types have been registered.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.name_to_ops.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_data_contracts::SchemaType;
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

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct TestActuator {
        command: String,
    }

    impl SchemaType for TestActuator {
        const NAME: &'static str = "test_actuator";
    }

    impl Streamable for TestActuator {}

    #[test]
    fn register_and_lookup_by_name() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();

        let ops = reg.get_by_name("test_sensor").unwrap();
        assert_eq!(ops.name, "test_sensor");
        assert_eq!(ops.type_id, TypeId::of::<TestSensor>());
    }

    #[test]
    fn register_and_resolve_type_id() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();

        assert_eq!(
            reg.resolve_name(&TypeId::of::<TestSensor>()),
            Some("test_sensor")
        );
        assert_eq!(reg.resolve_name(&TypeId::of::<u32>()), None);
    }

    #[test]
    fn serialize_roundtrip() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();

        let sensor = TestSensor {
            value: 42.5,
            timestamp: 1000,
        };

        let ops = reg.get_by_name("test_sensor").unwrap();
        let bytes = (ops.serialize)(&sensor).unwrap();
        let restored = (ops.deserialize)(&bytes).unwrap();
        let restored_sensor = restored.downcast_ref::<TestSensor>().unwrap();

        assert_eq!(restored_sensor.value, 42.5);
        assert_eq!(restored_sensor.timestamp, 1000);
    }

    #[test]
    fn duplicate_registration_is_idempotent() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();
        reg.register::<TestSensor>();

        assert_eq!(reg.known_names().len(), 1);
    }

    #[test]
    fn multiple_types_registered() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();
        reg.register::<TestActuator>();

        assert_eq!(reg.known_names().len(), 2);
        assert!(reg.get_by_name("test_sensor").is_some());
        assert!(reg.get_by_name("test_actuator").is_some());
    }

    #[test]
    fn empty_registry() {
        let reg = StreamableRegistry::new();
        assert!(reg.is_empty());
        assert!(reg.get_by_name("anything").is_none());
    }

    #[test]
    fn unknown_schema_returns_none() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>();

        assert!(reg.get_by_name("unknown_schema").is_none());
    }
}
