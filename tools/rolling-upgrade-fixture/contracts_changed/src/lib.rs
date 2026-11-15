#![no_std]
//! A peer built from a conflicting contracts revision: temperature@2 gained a field.
use aimdb_data_contracts::SchemaType;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemperatureV2 {
    pub celsius: f32,
    pub quality: u32,
    pub unexpected: u32,
}
impl SchemaType for TemperatureV2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}
aimdb_data_contracts::catalog! { contracts: [TemperatureV2], helpers: [], }
aimdb_data_contracts::migration_chain! { type Current = TemperatureV2; steps {} }
