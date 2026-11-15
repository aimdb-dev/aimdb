#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_data_contracts::VersionedInboundExt;
fn main() {
    let _ = aimdb_core::InboundLink::<contracts::V2>::new("mqtt://in").accepts_from::<0>();
}
