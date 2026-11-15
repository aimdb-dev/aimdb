#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
struct Humidity;
impl aimdb_data_contracts::SchemaType for Humidity { const NAME: &'static str = "humidity"; }
impl aimdb_data_contracts::ContractShape for Humidity {}
struct Decoder;
impl aimdb_data_contracts::PayloadDecoder for Decoder {
    fn decode_payload<T: serde::de::DeserializeOwned>(&self, _: &[u8]) -> Result<(T, usize), String> { unreachable!() }
}
fn main() {
    let _ = aimdb_data_contracts::LinkContract::<contracts::V2, _>::bound::<Humidity>(
        aimdb_core::envelope::PayloadCodec::Unspecified, Decoder,
    );
}
