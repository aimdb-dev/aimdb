//! MCP resources implementation
//!
//! Resources provide data that can be accessed by URI.

pub mod instances;
pub mod records;

// Re-export resource functions
pub use instances::{list_resources, read_resource};
