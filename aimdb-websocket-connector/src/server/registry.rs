//! Schema-name registry for [`Streamable`] types.
//!
//! Built incrementally via [`StreamableRegistry::register::<T>()`] at connector
//! construction time. It exists only to answer `list_topics` (resolve an
//! outbound topic's `TypeId` → schema name) and to reject schema-name
//! collisions. The actual record (de)serialization lives in the link routes'
//! serializer/deserializer, not here.

use std::any::TypeId;
use std::collections::HashMap;

use aimdb_data_contracts::Streamable;

/// Maps registered [`Streamable`] types to their schema names.
pub(crate) struct StreamableRegistry {
    /// Schema name → `TypeId` (collision detection on `register`).
    name_to_type_id: HashMap<&'static str, TypeId>,
    /// `TypeId` → schema name (outbound topic → schema for `list_topics`).
    type_id_to_name: HashMap<TypeId, &'static str>,
}

impl StreamableRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            name_to_type_id: HashMap::new(),
            type_id_to_name: HashMap::new(),
        }
    }

    /// Register a [`Streamable`] type. Idempotent for the same type; errors if a
    /// *different* type already claims the same schema name (`T::NAME`).
    pub fn register<T: Streamable>(&mut self) -> Result<(), String> {
        let type_id = TypeId::of::<T>();
        let name = T::NAME;
        match self.name_to_type_id.get(name) {
            Some(existing) if *existing == type_id => return Ok(()),
            Some(_) => {
                return Err(format!(
                    "schema name collision: \"{name}\" is already registered by a different type"
                ))
            }
            None => {}
        }
        self.name_to_type_id.insert(name, type_id);
        self.type_id_to_name.insert(type_id, name);
        Ok(())
    }

    /// Resolve a `TypeId` to its registered schema name.
    pub fn resolve_name(&self, type_id: &TypeId) -> Option<&'static str> {
        self.type_id_to_name.get(type_id).copied()
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
    fn register_and_resolve_name() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>().unwrap();
        assert_eq!(
            reg.resolve_name(&TypeId::of::<TestSensor>()),
            Some("test_sensor")
        );
    }

    #[test]
    fn unknown_type_resolves_to_none() {
        let reg = StreamableRegistry::new();
        assert_eq!(reg.resolve_name(&TypeId::of::<u32>()), None);
    }

    #[test]
    fn duplicate_registration_is_idempotent() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>().unwrap();
        reg.register::<TestSensor>().unwrap();
        assert_eq!(
            reg.resolve_name(&TypeId::of::<TestSensor>()),
            Some("test_sensor")
        );
    }

    #[test]
    fn name_collision_from_different_type_is_rejected() {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        struct FakeSensor {
            fake: bool,
        }

        impl SchemaType for FakeSensor {
            const NAME: &'static str = "test_sensor"; // same name as TestSensor
        }

        impl Streamable for FakeSensor {}

        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>().unwrap();

        let err = reg.register::<FakeSensor>().unwrap_err();
        assert!(err.contains("test_sensor"));
        assert!(err.contains("collision"));
    }

    #[test]
    fn multiple_types_registered() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>().unwrap();
        reg.register::<TestActuator>().unwrap();
        assert_eq!(
            reg.resolve_name(&TypeId::of::<TestSensor>()),
            Some("test_sensor")
        );
        assert_eq!(
            reg.resolve_name(&TypeId::of::<TestActuator>()),
            Some("test_actuator")
        );
    }
}
