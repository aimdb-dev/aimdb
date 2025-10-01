//! Build script for aimdb-embassy-adapter
//!
//! This build script enforces no_std compilation for the Embassy adapter.
//! Embassy is designed for embedded targets and should always be no_std.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Validate feature combinations for embedded target
    let std_enabled = env::var("CARGO_FEATURE_STD").is_ok();
    let _embassy_runtime_enabled = env::var("CARGO_FEATURE_EMBASSY_RUNTIME").is_ok();
    let metrics_enabled = env::var("CARGO_FEATURE_METRICS").is_ok();

    // Prevent std usage on embassy adapter
    if std_enabled {
        panic!(
            r#"
❌ Invalid feature combination: aimdb-embassy-adapter cannot use 'std' features

   Embassy adapter is designed exclusively for no_std embedded environments.

   Valid embassy features:
   • embassy-runtime  (core embassy runtime)
   • tracing         (no_std compatible tracing)

   For std features, use aimdb-tokio-adapter instead.
"#
        );
    }

    // Prevent std-only features on embedded
    if metrics_enabled {
        panic!(
            r#"
❌ Invalid feature: 'metrics' not supported on embedded targets

   Metrics collection requires std library support.

   Use aimdb-tokio-adapter for metrics collection.
"#
        );
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

        // Check for GNU ld specifically, avoiding false positives with lld or paths containing 'ld'
        let linker_name = linker
            .split('/')
            .next_back()
            .unwrap_or(&linker)
            .split('\\')
            .next_back()
            .unwrap_or(&linker);

        if linker_name == "ld" || linker_name.starts_with("arm-") && linker_name.ends_with("-ld") {
            println!("cargo:rustc-link-arg=-Wl,--gc-sections");
        }
        println!("cargo:rustc-cfg=embedded_target");
    }

    // Always available in no_std environments
    println!("cargo:rustc-cfg=core_available");
}
