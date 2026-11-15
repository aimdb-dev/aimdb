// A seed lock permits compilation before the lock test fills in new versions.
// Also proves that multiple versions of one name are not a name-ID collision.
use aimdb_data_contracts::{ContractShape, SchemaType};
#[derive(serde::Serialize, serde::Deserialize)]
struct V1;
#[derive(serde::Serialize, serde::Deserialize)]
struct V2;
impl SchemaType for V1 {
    const NAME: &'static str = "temperature";
}
impl SchemaType for V2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}
// This is a compile-only fixture: its seed is intentionally not a complete lock.
#[cfg(not(test))]
aimdb_data_contracts::catalog! {
    @lock_dir "seed",
    contracts: [V1, V2],
}
fn main() {
    #[cfg(not(test))]
    {
        assert_eq!(V1::SHAPE, None);
        assert_eq!(V2::SHAPE, None);
    }
}
