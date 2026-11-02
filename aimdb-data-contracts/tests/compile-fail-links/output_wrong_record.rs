use aimdb_core::{OutboundLink, RecordRegistrar};

#[derive(Clone, Debug)]
struct Temp;

#[derive(Clone, Debug)]
struct Other;

fn configure(reg: &mut RecordRegistrar<'_, Other>) {
    reg.output(OutboundLink::<Temp>::new("mqtt://out"));
}

fn main() {}
