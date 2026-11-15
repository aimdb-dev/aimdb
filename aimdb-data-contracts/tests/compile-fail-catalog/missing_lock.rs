use aimdb_data_contracts::SchemaType;
struct A;
impl SchemaType for A { const NAME: &'static str = "temperature"; }
aimdb_data_contracts::catalog! {
    @lock_dir "../fixtures/no_such_catalog",
    contracts: [A],
}
fn main() {}
