//! Contract verbs need a record whose key declares the contract: on a
//! `Name<T>` record (schema-free), they do not compile.
use aimdb_core::{AimDbBuilder, ConnectorHandle, Name};
use aimdb_data_contracts::{
    publish, subscribe, LinkCodecRegistrarExt, Linkable, LinkableRegistrarExt, Observable,
    ObservableRegistrarExt, SchemaType,
};

#[derive(Clone, Debug)]
struct Temp(f32);
impl SchemaType for Temp {
    const NAME: &'static str = "temp";
}
impl Linkable for Temp {
    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
    fn from_bytes(_: &[u8]) -> Result<Self, String> {
        Ok(Temp(0.0))
    }
}
impl Observable for Temp {
    type Signal = f32;
    const UNIT: &'static str = "C";
    fn signal(&self) -> f32 {
        self.0
    }
}

const TEMP: Name<Temp> = Name::new("temp");

fn wire(b: &mut AimDbBuilder, mqtt: ConnectorHandle) {
    b.configure(TEMP, |r| {
        r.linked_to("mqtt://out");
        r.linked_from("mqtt://in");
        r.linked_to_with("mqtt://out2", aimdb_data_contracts::link_codecs::Default);
        r.observe();
        r.output(publish(mqtt, "out"));
        r.input(subscribe(mqtt, "in"));
    });
}

fn main() {}
