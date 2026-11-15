#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
const READING: aimdb_data_contracts::Key<contracts::V2> = aimdb_data_contracts::Key::new("reading");
fn wrong(b: &mut aimdb_core::AimDbBuilder) {
    b.configure(READING, |r| {
        r.transform(READING, |b| b.map(|v: &contracts::V1| Some(contracts::V2 { celsius: v.temp, quality: 1 })));
    });
}
fn main() {}
