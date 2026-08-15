//! Schema-name registry for [`Streamable`] types.
//!
//! Built incrementally via [`StreamableRegistry::register::<T>()`] at connector
//! construction time. It resolves a record's `TypeId` → schema name so the
//! dispatch can stamp schema names onto `record.list` rows, and rejects
//! schema-name collisions. The actual record (de)serialization lives in the link
//! routes' serializer/deserializer, not here.

use std::any::TypeId;
use std::collections::HashMap;

use aimdb_data_contracts::Streamable;

/// Maps registered [`Streamable`] types to their schema names.
pub(crate) struct StreamableRegistry {
    /// Schema name → `TypeId` (collision detection on `register`).
    name_to_type_id: HashMap<&'static str, TypeId>,
    /// `TypeId` → schema name (resolved onto `record.list` rows).
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

    /// Every registered schema keyed by the record's `type_id` string (matching
    /// `RecordMetadata::type_id`), so the dispatch can stamp schema names onto
    /// the `record.list` rows it gets back from core.
    pub fn schema_by_type_id(&self) -> HashMap<String, String> {
        self.type_id_to_name
            .iter()
            .map(|(type_id, name)| (format!("{type_id:?}"), name.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_data_contracts::SchemaType;
    use serde::{Deserialize, Serialize};

    /// Look up a type's resolved schema name via the public map, keyed the same
    /// way `RecordMetadata::type_id` is rendered.
    fn resolved<T: 'static>(reg: &StreamableRegistry) -> Option<String> {
        reg.schema_by_type_id()
            .get(&format!("{:?}", TypeId::of::<T>()))
            .cloned()
    }

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
        assert_eq!(resolved::<TestSensor>(&reg).as_deref(), Some("test_sensor"));
    }

    #[test]
    fn unknown_type_resolves_to_none() {
        let reg = StreamableRegistry::new();
        assert_eq!(resolved::<u32>(&reg), None);
    }

    #[test]
    fn duplicate_registration_is_idempotent() {
        let mut reg = StreamableRegistry::new();
        reg.register::<TestSensor>().unwrap();
        reg.register::<TestSensor>().unwrap();
        assert_eq!(resolved::<TestSensor>(&reg).as_deref(), Some("test_sensor"));
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
        assert_eq!(resolved::<TestSensor>(&reg).as_deref(), Some("test_sensor"));
        assert_eq!(
            resolved::<TestActuator>(&reg).as_deref(),
            Some("test_actuator")
        );
    }
}
