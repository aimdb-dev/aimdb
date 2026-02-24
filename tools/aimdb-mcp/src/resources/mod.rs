//! MCP resources implementation
//!
//! Resources provide data that can be accessed by URI.

pub mod architecture;
pub mod instances;
pub mod records;

use crate::error::McpResult;
use crate::protocol::{ResourceReadResult, ResourcesListResult};

/// List all available resources (instances + architecture)
pub async fn list_resources() -> McpResult<ResourcesListResult> {
    let mut result = instances::list_resources().await?;
    // Append architecture resources
    for r in architecture::list_resources() {
        result.resources.push(r);
    }
    Ok(result)
}

/// Read a resource by URI (instances first, then architecture)
pub async fn read_resource(uri: &str) -> McpResult<ResourceReadResult> {
    if uri.starts_with("aimdb://architecture") {
        architecture::read_resource(uri)
    } else {
        instances::read_resource(uri).await
    }
}
