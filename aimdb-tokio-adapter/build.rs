//! Build script for aimdb-tokio-adapter
//!
//! This build script validates feature flag combinations for the Tokio adapter
//! and ensures proper std platform requirements.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TOKIO_RUNTIME");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_METRICS");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TEST_UTILS");

    // Get enabled features
    let tokio_runtime_enabled = env::var("CARGO_FEATURE_TOKIO_RUNTIME").is_ok();
    let metrics_enabled = env::var("CARGO_FEATURE_METRICS").is_ok();
    let test_utils_enabled = env::var("CARGO_FEATURE_TEST_UTILS").is_ok();

    // Validate observability features require tokio-runtime
    if metrics_enabled && !tokio_runtime_enabled {
        panic!(
            r#"
❌ Invalid feature combination: 'metrics' requires 'tokio-runtime'

   Metrics collection depends on Tokio async runtime.

   Use: features = ["tokio-runtime", "metrics"]
"#
        );
    }

    if test_utils_enabled && !tokio_runtime_enabled {
        panic!(
            r#"
❌ Invalid feature combination: 'test-utils' requires 'tokio-runtime'

   Test utilities depend on Tokio async runtime.

   Use: features = ["tokio-runtime", "test-utils"]
"#
        );
    }

    // Set conditional compilation flags
    if tokio_runtime_enabled {
        println!("cargo:rustc-cfg=feature_tokio");
    }

    println!("✅ Tokio adapter feature validation passed");
}
