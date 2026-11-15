fn main() {
    let mut b = aimdb_core::AimDbBuilder::new();
    let name = String::from("reading");
    let key = aimdb_core::Name::<u32>::new(&name);
    b.configure(key, |_| {});
}
