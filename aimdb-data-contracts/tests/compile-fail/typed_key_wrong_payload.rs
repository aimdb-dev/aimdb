#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
const READING: aimdb_data_contracts::Key<contracts::V2> = aimdb_data_contracts::Key::new("reading");
fn wrong(db: &aimdb_core::AimDb) {
    let _: aimdb_core::Producer<contracts::V1> = db.producer(READING).unwrap();
}
fn main() {}
