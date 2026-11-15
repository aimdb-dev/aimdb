use aimdb_data_contracts::{migration_chain, ContractShape, Reaches, SchemaType};
#[derive(serde::Serialize, serde::Deserialize)]
struct Sensor { value: u32 }
impl SchemaType for Sensor { const NAME: &'static str = "sensor"; const VERSION: u32 = 2; }
impl ContractShape for Sensor {}
migration_chain! { type Current = Sensor; steps {} }
fn requires_v1<T: Reaches<1>>() {}
fn main() { requires_v1::<Sensor>(); }
