use aimdb_data_contracts::migration_chain;
migration_chain! {
    type Current = Sensor;
    version_field = "schema_version";
    steps {}
}
fn main() {}
