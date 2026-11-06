//! MCP resources implementation
//!
//! Resources provide data that can be accessed by URI.

pub mod instances;
pub mod records;

use crate::error::McpResult;
use crate::protocol::{ResourceReadResult, ResourcesListResult};

/// List all available resources
pub async fn list_resources() -> McpResult<ResourcesListResult> {
    instances::list_resources().await
}

/// Read a resource by URI
pub async fn read_resource(uri: &str) -> McpResult<ResourceReadResult> {
    instances::read_resource(uri).await
}
