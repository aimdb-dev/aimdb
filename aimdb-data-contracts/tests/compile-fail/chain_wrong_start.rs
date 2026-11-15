// First (only) step doesn't start at version 1.
use aimdb_data_contracts::{migration_chain, MigrationError, MigrationStep, SchemaType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct WrongStartV2 {}
impl aimdb_data_contracts::ContractShape for WrongStartV2 {}
impl SchemaType for WrongStartV2 {
    const NAME: &'static str = "wrong_start";
    const VERSION: u32 = 2;
}

#[derive(Clone, Serialize, Deserialize)]
struct WrongStartCurrent {}
impl aimdb_data_contracts::ContractShape for WrongStartCurrent {}
impl SchemaType for WrongStartCurrent {
    const NAME: &'static str = "wrong_start";
    const VERSION: u32 = 3;
}

struct WrongStartStep;
impl MigrationStep for WrongStartStep {
    type Older = WrongStartV2;
    type Newer = WrongStartCurrent;
    const FROM_VERSION: u32 = 2;
    const TO_VERSION: u32 = 3;

    fn up(_older: WrongStartV2) -> Result<WrongStartCurrent, MigrationError> {
        Ok(WrongStartCurrent {})
    }
    fn down(_newer: WrongStartCurrent) -> Result<WrongStartV2, MigrationError> {
        Ok(WrongStartV2 {})
    }
}

migration_chain! {
    type Current = WrongStartCurrent;
    steps {
        WrongStartStep: WrongStartV2 => WrongStartCurrent,
    }
}

fn main() {}
