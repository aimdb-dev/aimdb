// Step increments the version by more than 1 (skips version 2 entirely).
use aimdb_data_contracts::{
    migration_chain, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct GapV1 {
    schema_version: u32,
}
impl SchemaType for GapV1 {
    const NAME: &'static str = "gap";
    const VERSION: u32 = 1;
}

#[derive(Clone, Serialize, Deserialize)]
struct GapCurrent {
    schema_version: u32,
}
impl SchemaType for GapCurrent {
    const NAME: &'static str = "gap";
    const VERSION: u32 = 3;
}

struct GapStep;
impl MigrationStep for GapStep {
    type Older = GapV1;
    type Newer = GapCurrent;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 3;

    fn up(_older: GapV1) -> Result<GapCurrent, MigrationError> {
        Ok(GapCurrent { schema_version: 3 })
    }
    fn down(_newer: GapCurrent) -> Result<GapV1, MigrationError> {
        Ok(GapV1 { schema_version: 1 })
    }
}

migration_chain! {
    type Current = GapCurrent;
    version_field = "schema_version";
    steps {
        GapStep: GapV1 => GapCurrent,
    }
}

fn main() {}
