// An older type keeps a different contract name than Current.
use aimdb_data_contracts::{migration_chain, MigrationError, MigrationStep, SchemaType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct OtherNameV1 {}
impl aimdb_data_contracts::ContractShape for OtherNameV1 {}
impl SchemaType for OtherNameV1 {
    const NAME: &'static str = "other_name_v1";
    const VERSION: u32 = 1;
}

#[derive(Clone, Serialize, Deserialize)]
struct OtherNameCurrent {}
impl aimdb_data_contracts::ContractShape for OtherNameCurrent {}
impl SchemaType for OtherNameCurrent {
    const NAME: &'static str = "other_name";
    const VERSION: u32 = 2;
}

struct OtherNameStep;
impl MigrationStep for OtherNameStep {
    type Older = OtherNameV1;
    type Newer = OtherNameCurrent;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(_older: OtherNameV1) -> Result<OtherNameCurrent, MigrationError> {
        Ok(OtherNameCurrent {})
    }
    fn down(_newer: OtherNameCurrent) -> Result<OtherNameV1, MigrationError> {
        Ok(OtherNameV1 {})
    }
}

migration_chain! {
    type Current = OtherNameCurrent;
    steps {
        OtherNameStep: OtherNameV1 => OtherNameCurrent,
    }
}

fn main() {}
