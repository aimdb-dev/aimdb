use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Get the output directory
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build_dir = out_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("build");

    // CRITICAL FIX: knx-pico generates memory.x for RP2350 (flash at 0x10000000)
    // This conflicts with STM32H5 (flash at 0x08000000) from embassy-stm32
    // We MUST remove knx-pico's memory.x to use the correct STM32 memory layout
    if let Ok(entries) = fs::read_dir(&build_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Remove knx-pico's conflicting memory.x
            if name.starts_with("knx-pico-") {
                let knx_memory = path.join("out").join("memory.x");
                if knx_memory.exists() {
                    let _ = fs::remove_file(&knx_memory);
                    println!(
                        "cargo:warning=Removed conflicting RP2350 memory.x from knx-pico (using STM32H5 layout)"
                    );
                }
            }
        }
    }

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
    println!("cargo:rerun-if-changed=build.rs");
}
