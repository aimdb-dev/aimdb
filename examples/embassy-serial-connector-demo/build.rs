fn main() {
    // The build-info section: composed here, placed by
    // `aimdb_build_info::section!()` in `main`, served by `instance.info`.
    aimdb_build_info::BuildInfo::from_cargo_env()
        .ext("board", "nucleo-h563zi")
        .write()
        .expect("build info");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    // The build-info section sits at a fixed address between the vector table
    // (0x24c bytes on STM32H5) and the code, so a debug probe finds it without
    // a server running. Up to 7 KiB fit before `.text`.
    println!("cargo:rustc-link-arg-bins=--defsym=AIMDB_BUILD_INFO_ADDR=0x08000400");
    // Code starts at the second 8 KiB flash sector, so the section's address
    // depends on neither its size nor the code's. cortex-m-rt `PROVIDE`s
    // `_stext`, so defining it here wins; the address also keeps `.text`
    // 8-aligned, which rust-lld otherwise reports and CI denies.
    println!("cargo:rustc-link-arg-bins=--defsym=_stext=0x08002000");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Taimdb_build_info.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
