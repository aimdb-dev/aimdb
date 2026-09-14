fn main() {
    println!("cargo:rustc-link-arg-bins=--nmagic");
    // cortex-m-rt places `.text` at an explicit `_stext`, which its linker
    // script derives as the byte after the vector table. On STM32H5 that table
    // is 0x24c bytes, so `.text` would land 4-byte aligned while its contents
    // ask for 8 — which rust-lld reports, and CI denies. `_stext` is
    // `PROVIDE`d, so defining it here wins; round it up to the next multiple
    // of 8. A chip whose vector table outgrows this fails the link with a
    // section overlap rather than misplacing code silently.
    println!("cargo:rustc-link-arg-bins=--defsym=_stext=0x8000250");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
