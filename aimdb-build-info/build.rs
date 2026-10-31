//! Ships the board linker fragment: copies it to `OUT_DIR` and puts `OUT_DIR`
//! on the link search path, as cortex-m-rt does for `link.x`. A board's build
//! script then only adds `-Taimdb_build_info.x`.

use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    fs::write(
        out.join("aimdb_build_info.x"),
        include_bytes!("link/aimdb_build_info.x"),
    )
    .expect("write aimdb_build_info.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=link/aimdb_build_info.x");
    println!("cargo:rerun-if-changed=build.rs");
}
