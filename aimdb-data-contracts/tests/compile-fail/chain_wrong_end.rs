// Last (only) step doesn't end at Current::VERSION.
use aimdb_data_contracts::{
    migration_chain, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct WrongEndV1 {
    schema_version: u32,
}
impl SchemaType for WrongEndV1 {
    const NAME: &'static str = "wrong_end";
    const VERSION: u32 = 1;
}

#[derive(Clone, Serialize, Deserialize)]
struct WrongEndCurrent {
    schema_version: u32,
}
impl SchemaType for WrongEndCurrent {
    const NAME: &'static str = "wrong_end";
    const VERSION: u32 = 5;
}

struct WrongEndStep;
impl MigrationStep for WrongEndStep {
    type Older = WrongEndV1;
    type Newer = WrongEndCurrent;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(_older: WrongEndV1) -> Result<WrongEndCurrent, MigrationError> {
        Ok(WrongEndCurrent { schema_version: 5 })
    }
    fn down(_newer: WrongEndCurrent) -> Result<WrongEndV1, MigrationError> {
        Ok(WrongEndV1 { schema_version: 1 })
    }
}

migration_chain! {
    type Current = WrongEndCurrent;
    version_field = "schema_version";
    steps {
        WrongEndStep: WrongEndV1 => WrongEndCurrent,
    }
}

fn main() {}
