use aimdb_core::{OutboundLink, RecordRegistrar};

#[derive(Clone, Debug)]
struct Temp;

fn configure(reg: &mut RecordRegistrar<'_, Temp>) {
    reg.input(OutboundLink::<Temp>::new("mqtt://out"));
}

fn main() {}
