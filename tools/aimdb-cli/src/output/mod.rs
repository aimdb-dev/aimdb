//! Output Formatting
//!
//! This module provides various output formatting options for CLI results.

pub mod json;
pub mod live;
pub mod table;

use aimdb_client::{Stamp, Stamped};

/// A write stamp as seconds since the instance started, to the microsecond
/// (`81.234567`). Stamps are only comparable within one instance lifetime.
pub fn format_ts(ts: Stamp) -> String {
    let micros = ts.as_nanos() / 1_000;
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// A value with its stamp in the AimX 4.0 wire shape, `{"value", "ts"}`, for
/// the JSON and YAML formats.
pub fn stamped_json(sample: &Stamped<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "value": sample.value, "ts": sample.stamp().as_nanos() })
}

/// Output format selection
#[derive(Default, Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table format
    #[default]
    Table,
    /// Pretty-printed JSON
    Json,
    /// Compact JSON (one line)
    JsonCompact,
    /// YAML format
    #[cfg(feature = "yaml")]
    Yaml,
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
