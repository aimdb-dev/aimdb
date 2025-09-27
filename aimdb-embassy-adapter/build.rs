//! Build script for aimdb-embassy-adapter
//!
//! This build script enforces no_std compilation for the Embassy adapter.
//! Embassy is designed for embedded targets and should always be no_std.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Check if std feature is enabled via feature unification
    let features = env::var("CARGO_FEATURE_STD").is_ok();

    if features {
        println!("cargo:warning=aimdb-embassy-adapter: Skipping implementation due to std feature");
        println!(
            "cargo:warning=This crate is designed exclusively for no_std embedded environments"
        );
        println!("cargo:warning=To build embassy adapter: cd aimdb-embassy-adapter && cargo build");
    }

    // Always enforce no_std compilation
    println!("cargo:rustc-cfg=no_std_enforced");

    // Set target-specific configurations for embedded targets
    let target = env::var("TARGET").unwrap_or_default();

    if target.contains("thumbv") || target.contains("riscv") || target.contains("arm") {
        // For embedded targets, ensure optimizations that help with code size
        // Only add --gc-sections if using GNU ld as the linker
        let linker_env = format!(
            "CARGO_TARGET_{}_LINKER",
            target.replace('-', "_").to_uppercase()
        );
        let linker = env::var(&linker_env)
            .or_else(|_| env::var("RUSTC_LINKER"))
            .unwrap_or_default();
        if linker.contains("ld") {
            println!("cargo:rustc-link-arg=-Wl,--gc-sections");
        }
        println!("cargo:rustc-cfg=embedded_target");
    }

    // Always available in no_std environments
    println!("cargo:rustc-cfg=core_available");
}
