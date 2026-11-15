#[derive(aimdb_derive::RecordKey, Clone, Copy, Debug, PartialEq, Eq)]
#[record_key(payload = u32)]
#[key_prefix = "reading."]
enum Record { #[key = "value"] Value }
fn main() {}
