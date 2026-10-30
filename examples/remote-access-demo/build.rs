fn main() {
    // The build-info section the server places with
    // `aimdb_build_info::section!()` and serves as `instance.info`.
    aimdb_build_info::BuildInfo::from_cargo_env()
        .kind("demo")
        .ext("board", "host")
        .write()
        .expect("build info");
}
