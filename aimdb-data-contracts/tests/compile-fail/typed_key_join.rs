#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
const READING: aimdb_data_contracts::Key<contracts::V2> = aimdb_data_contracts::Key::new("reading");
fn wrong(join: aimdb_core::transform::JoinBuilder<contracts::V2>) {
    let _ = join.input::<contracts::V1>(READING);
}
fn main() {}
