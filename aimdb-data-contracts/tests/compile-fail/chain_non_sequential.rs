// Each step individually increments by 1, and the types chain correctly
// (SeqStep1::Newer == SeqStep2::Older == SeqV2), but SeqStep2's declared
// FROM_VERSION (3) doesn't match SeqStep1's declared TO_VERSION (2) — a
// version-numbering gap the type system alone can't catch.
use aimdb_data_contracts::{migration_chain, MigrationError, MigrationStep, SchemaType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct SeqV1 {}
impl aimdb_data_contracts::ContractShape for SeqV1 {}
impl SchemaType for SeqV1 {
    const NAME: &'static str = "seq";
    const VERSION: u32 = 1;
}

#[derive(Clone, Serialize, Deserialize)]
struct SeqV2 {}
impl aimdb_data_contracts::ContractShape for SeqV2 {}
impl SchemaType for SeqV2 {
    const NAME: &'static str = "seq";
    const VERSION: u32 = 2;
}

#[derive(Clone, Serialize, Deserialize)]
struct SeqCurrent {}
impl aimdb_data_contracts::ContractShape for SeqCurrent {}
impl SchemaType for SeqCurrent {
    const NAME: &'static str = "seq";
    const VERSION: u32 = 4;
}

struct SeqStep1;
impl MigrationStep for SeqStep1 {
    type Older = SeqV1;
    type Newer = SeqV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(_older: SeqV1) -> Result<SeqV2, MigrationError> {
        Ok(SeqV2 {})
    }
    fn down(_newer: SeqV2) -> Result<SeqV1, MigrationError> {
        Ok(SeqV1 {})
    }
}

// Types chain fine (Older == SeqStep1::Newer), but the declared
// FROM_VERSION (3) doesn't match SeqStep1's TO_VERSION (2).
struct SeqStep2;
impl MigrationStep for SeqStep2 {
    type Older = SeqV2;
    type Newer = SeqCurrent;
    const FROM_VERSION: u32 = 3;
    const TO_VERSION: u32 = 4;

    fn up(_older: SeqV2) -> Result<SeqCurrent, MigrationError> {
        Ok(SeqCurrent {})
    }
    fn down(_newer: SeqCurrent) -> Result<SeqV2, MigrationError> {
        Ok(SeqV2 {})
    }
}

migration_chain! {
    type Current = SeqCurrent;
    steps {
        SeqStep1: SeqV1 => SeqV2,
        SeqStep2: SeqV2 => SeqCurrent,
    }
}

fn main() {}
