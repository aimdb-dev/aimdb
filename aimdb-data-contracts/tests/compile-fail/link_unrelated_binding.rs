#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_data_contracts::EnvelopeLinkExt;
struct Humidity;
impl aimdb_data_contracts::SchemaType for Humidity { const NAME: &'static str = "humidity"; }
impl aimdb_data_contracts::ContractShape for Humidity {}
fn main() {
    let _ = aimdb_core::InboundLink::<contracts::V2>::new("mqtt://in").with_contract::<Humidity>();
}
