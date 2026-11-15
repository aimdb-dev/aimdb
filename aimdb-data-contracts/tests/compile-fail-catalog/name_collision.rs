use aimdb_data_contracts::SchemaType;
struct A;
struct B;
impl SchemaType for A { const NAME: &'static str = "costarring"; }
impl SchemaType for B { const NAME: &'static str = "liquid"; }
aimdb_data_contracts::catalog! {
    @lock_dir "../fixtures/seed",
    contracts: [A, B],
}
fn main() {}
