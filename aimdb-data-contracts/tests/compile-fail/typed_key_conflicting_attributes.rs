#[derive(aimdb_derive::RecordKey, Clone, Copy, Debug, PartialEq, Eq)]
#[record_key(payload = u32, contract = u32)]
enum Record { #[key = "reading"] Reading }
fn main() {}
