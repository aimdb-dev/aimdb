use aimdb_data_contracts::{
    migration_chain, ContractShape, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct V1 {
    pub temp: f32,
}
impl SchemaType for V1 {
    const NAME: &'static str = "temperature";
}
impl ContractShape for V1 {
    const SHAPE: Option<u64> = Some(0x1234);
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct V2 {
    pub celsius: f32,
    pub quality: u32,
}
impl SchemaType for V2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}
impl ContractShape for V2 {
    const SHAPE: Option<u64> = Some(0x5678);
}
pub struct Step;
impl MigrationStep for Step {
    type Older = V1;
    type Newer = V2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;
    fn up(v: V1) -> Result<V2, MigrationError> {
        if v.temp < -273.15 {
            return Err(MigrationError::ConversionFailed("below absolute zero"));
        }
        Ok(V2 {
            celsius: v.temp,
            quality: 1,
        })
    }
    fn down(v: V2) -> Result<V1, MigrationError> {
        Ok(V1 { temp: v.celsius })
    }
}
migration_chain! { type Current = V2; steps { Step: V1 => V2 } }
