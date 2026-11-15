#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
struct Decoder;
impl aimdb_data_contracts::PayloadDecoder for Decoder {
    fn decode_payload<T: serde::de::DeserializeOwned>(&self, _: &[u8]) -> Result<(T, usize), String> { unreachable!() }
}
fn main() {
    let _ = aimdb_data_contracts::LinkContract::<contracts::V2, _>::inline_from::<0>(
        aimdb_core::envelope::PayloadCodec::Unspecified, Decoder,
    );
}
