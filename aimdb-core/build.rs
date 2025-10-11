//! Build script for aimdb-core
//!
//! This build script validates feature flag combinations to prevent
//! impossible or conflicting configurations at compile time.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STD");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBEDDED");

    // Get enabled features
    let std_enabled = env::var("CARGO_FEATURE_STD").is_ok();
    let tokio_runtime_enabled = env::var("CARGO_FEATURE_TOKIO_RUNTIME").is_ok();
    let embassy_runtime_enabled = env::var("CARGO_FEATURE_EMBASSY_RUNTIME").is_ok();
    let metrics_enabled = env::var("CARGO_FEATURE_METRICS").is_ok();

    // Note: no_std is the absence of std feature, no validation needed for mutual exclusion

    // Validate runtime feature combinations (skip validation when both are enabled via --all-features for testing)
    if tokio_runtime_enabled && embassy_runtime_enabled {
        // Allow this for --all-features testing, but warn
        eprintln!("⚠️  Warning: Both tokio-runtime and embassy-runtime enabled (likely from --all-features)");
        eprintln!("   This is only valid for testing. Production builds should use one runtime.");
    }

    // Validate metrics require std
    if metrics_enabled && !std_enabled {
        panic!(
            r#"
❌ Invalid feature combination: 'metrics' requires 'std' platform

   Metrics collection requires standard library support.

   Use: features = ["std", "metrics"]
"#
        );
    }

    // Validate runtime dependencies
    if tokio_runtime_enabled && !std_enabled {
        panic!(
            r#"
❌ Invalid feature combination: 'tokio-runtime' requires 'std' platform

   Tokio runtime depends on standard library.

   Use: features = ["std", "tokio-runtime"]
"#
        );
    }

    if embassy_runtime_enabled && std_enabled {
        panic!(
            r#"
❌ Invalid feature combination: 'embassy-runtime' conflicts with 'std'

   Embassy runtime is designed for no_std environments.

   Use: features = ["embassy-runtime"] (without std)
   Or:  features = ["embedded"] (convenience alias)
"#
        );
    }

    // Set conditional compilation flags
    if std_enabled {
        println!("cargo:rustc-cfg=feature_std");
    } else {
        println!("cargo:rustc-cfg=feature_no_std");
    }

    // Platform-specific optimizations
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("thumbv") || target.contains("riscv") || target.contains("arm") {
        println!("cargo:rustc-cfg=embedded_target");
        // Enable link-time optimizations for embedded targets
        if target.contains("thumb") {
            println!("cargo:rustc-link-arg=-Wl,--gc-sections");
        }
    }

    println!("✅ AimDB feature validation passed");
}
