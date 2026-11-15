use aimdb_data_contracts::{migration_chain, ContractShape, SchemaType, VersionOf};
#[derive(serde::Serialize, serde::Deserialize)]
struct Sensor { value: u32 }
impl SchemaType for Sensor { const NAME: &'static str = "sensor"; }
impl ContractShape for Sensor {}
struct Humidity;
migration_chain! { type Current = Sensor; steps {} }
fn bind<U: VersionOf<Sensor>>() {}
fn main() { bind::<Humidity>(); }
