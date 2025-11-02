//! Output Formatting
//!
//! This module provides various output formatting options for CLI results.

pub mod json;
pub mod live;
pub mod table;

/// Output format selection
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table format
    Table,
    /// Pretty-printed JSON
    Json,
    /// Compact JSON (one line)
    JsonCompact,
    /// YAML format
    #[cfg(feature = "yaml")]
    Yaml,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Table
    }
}
