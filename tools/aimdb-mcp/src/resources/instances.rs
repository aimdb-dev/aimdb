//! Instances resource implementation
//!
//! Provides the `aimdb://instances` resource that lists all discovered
//! AimDB instances with their metadata.

use crate::error::{McpError, McpResult};
use crate::protocol::{Resource, ResourceContent, ResourceReadResult, ResourcesListResult};
use serde_json::json;
use tracing::debug;

/// URI for the instances resource
pub const INSTANCES_URI: &str = "aimdb://instances";

/// List all available resources
///
/// Returns:
/// - `aimdb://instances` - List of all discovered instances
/// - `aimdb://<socket>/records` - Records for each discovered instance
pub async fn list_resources() -> McpResult<ResourcesListResult> {
    debug!("📋 Listing resources");

    let mut resources = vec![Resource {
        uri: INSTANCES_URI.to_string(),
        name: "AimDB Instances".to_string(),
        description: Some("List of all discovered AimDB instances with metadata".to_string()),
        mime_type: Some("application/json".to_string()),
    }];

    // Add records resources for each discovered instance
    match super::records::list_records_resources().await {
        Ok(mut records_resources) => {
            resources.append(&mut records_resources);
        }
        Err(e) => {
            // If discovery fails, just log it and continue with instances resource only
            debug!("Could not list records resources: {}", e);
        }
    }

    Ok(ResourcesListResult { resources })
}

/// Read a resource by URI
///
/// Currently supports:
/// - `aimdb://instances` - List all discovered instances
/// - `aimdb://<socket>/records` - List records for specific instance
pub async fn read_resource(uri: &str) -> McpResult<ResourceReadResult> {
    debug!("📖 Reading resource: {}", uri);

    match uri {
        INSTANCES_URI => read_instances_resource().await,
        _ if super::records::is_records_uri(uri) => {
            // Extract socket path and read records
            if let Some(socket_path) = super::records::extract_socket_path(uri) {
                super::records::read_records_resource(&socket_path).await
            } else {
                Err(McpError::InvalidParams(format!(
                    "Invalid records URI: {}",
                    uri
                )))
            }
        }
        _ => Err(McpError::InvalidParams(format!(
            "Unknown resource URI: {}",
            uri
        ))),
    }
}

/// Read the instances resource
///
/// Discovers all running AimDB instances and returns their metadata as JSON.
async fn read_instances_resource() -> McpResult<ResourceReadResult> {
    debug!("🔍 Discovering instances for resource");

    // Use aimdb_client discovery
    let instances = aimdb_client::discover_instances().await?;

    debug!("✅ Found {} instance(s)", instances.len());

    // Convert to JSON
    let instances_json: Vec<_> = instances
        .into_iter()
        .map(|info| {
            json!({
                "socket_path": info.socket_path.display().to_string(),
                "server_version": info.server_version,
                "protocol_version": info.protocol_version,
                "permissions": info.permissions,
                "writable_records": info.writable_records,
                "max_subscriptions": info.max_subscriptions,
                "authenticated": info.authenticated,
            })
        })
        .collect();

    let content_json = json!({
        "instances": instances_json,
        "count": instances_json.len(),
    });

    let content = ResourceContent {
        uri: INSTANCES_URI.to_string(),
        mime_type: Some("application/json".to_string()),
        text: Some(serde_json::to_string_pretty(&content_json)?),
        blob: None,
    };

    Ok(ResourceReadResult {
        contents: vec![content],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_resources() {
        let result = list_resources().await.unwrap();
        // Should have at least the instances resource
        assert!(!result.resources.is_empty());

        // First resource should be instances
        assert_eq!(result.resources[0].uri, INSTANCES_URI);
        assert_eq!(result.resources[0].name, "AimDB Instances");

        // If we have more resources, they should be records resources
        for resource in result.resources.iter().skip(1) {
            assert!(resource.uri.starts_with("aimdb://"));
            assert!(resource.uri.ends_with("/records"));
        }
    }

    #[tokio::test]
    async fn test_read_unknown_resource() {
        let result = read_resource("aimdb://unknown").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_instances_resource() {
        // This test will succeed if instances are found, or fail gracefully if not
        let result = read_resource(INSTANCES_URI).await;

        match result {
            Ok(read_result) => {
                assert_eq!(read_result.contents.len(), 1);
                assert_eq!(read_result.contents[0].uri, INSTANCES_URI);
                assert!(read_result.contents[0].text.is_some());
            }
            Err(err) => {
                // Should fail with "No running AimDB instances found"
                assert!(err.message().contains("No running AimDB instances"));
            }
        }
    }
}
