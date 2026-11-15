#[path = "../fixtures/admission_contracts.rs"]
mod contracts;
const READING: aimdb_data_contracts::Key<contracts::V2> = aimdb_data_contracts::Key::new("reading");
fn wrong(db: &aimdb_core::AimDb, b: &mut aimdb_core::AimDbBuilder) {
    b.configure(READING, |_: &mut aimdb_core::RecordRegistrar<'_, contracts::V1>| {});
    let _: aimdb_core::Consumer<contracts::V1> = db.consumer(READING).unwrap();
    let _ = db.produce(READING, contracts::V1 { temp: 1.0 });
    let _: Option<aimdb_core::Stamped<contracts::V1>> = db.latest(READING).unwrap();
    let _: aimdb_core::buffer::Reader<contracts::V1> = db.subscribe(READING).unwrap();
    b.configure(READING, |r| {
        r.link_to("test://out").with_serializer(|_, _: &contracts::V1| Ok(Vec::new())).finish();
    });
}
fn main() {}
