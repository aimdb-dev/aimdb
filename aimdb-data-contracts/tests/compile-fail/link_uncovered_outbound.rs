#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_data_contracts::VersionedOutboundExt;
fn main() {
    let _ = aimdb_core::OutboundLink::<contracts::V2>::new("mqtt://out").sends_as::<0>();
}
