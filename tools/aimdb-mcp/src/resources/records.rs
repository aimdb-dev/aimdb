//! Records resource implementation
//!
//! Provides the `aimdb://<socket>/records` resource that lists all records
//! for a specific AimDB instance.

use crate::error::{McpError, McpResult};
use crate::protocol::{Resource, ResourceContent, ResourceReadResult};
use aimdb_client::AimxClient;
use serde_json::json;
use std::path::Path;
use tracing::debug;

/// URI prefix for records resources
pub const RECORDS_URI_PREFIX: &str = "aimdb://";
pub const RECORDS_URI_SUFFIX: &str = "/records";

/// Generate records resource for a specific socket path
pub fn records_resource_for_socket(socket_path: &str) -> Resource {
    let uri = format!(
        "{}{}{}",
        RECORDS_URI_PREFIX, socket_path, RECORDS_URI_SUFFIX
    );

    Resource {
        uri,
        name: format!("Records: {}", socket_path),
        description: Some(format!(
            "List of all records in the AimDB instance at {}",
            socket_path
        )),
        mime_type: Some("application/json".to_string()),
    }
}

/// Check if a URI is a records resource URI
pub fn is_records_uri(uri: &str) -> bool {
    uri.starts_with(RECORDS_URI_PREFIX) && uri.ends_with(RECORDS_URI_SUFFIX)
}

/// Extract socket path from a records resource URI
pub fn extract_socket_path(uri: &str) -> Option<String> {
    if !is_records_uri(uri) {
        return None;
    }

    // Remove prefix and suffix
    let path = uri
        .strip_prefix(RECORDS_URI_PREFIX)?
        .strip_suffix(RECORDS_URI_SUFFIX)?;

    Some(path.to_string())
}

/// List records resources for all discovered instances
pub async fn list_records_resources() -> McpResult<Vec<Resource>> {
    debug!("🔍 Discovering instances for records resources");

    // Use aimdb_client discovery
    let instances = aimdb_client::discover_instances().await?;

    debug!("✅ Found {} instance(s)", instances.len());

    // Generate a resource for each instance
    let resources: Vec<Resource> = instances
        .into_iter()
        .map(|info| records_resource_for_socket(&info.socket_path.display().to_string()))
        .collect();

    Ok(resources)
}

/// Read the records resource for a specific instance
pub async fn read_records_resource(socket_path: &str) -> McpResult<ResourceReadResult> {
    debug!("📋 Reading records for socket: {}", socket_path);

    // Validate socket path exists
    if !Path::new(socket_path).exists() {
        return Err(McpError::InvalidParams(format!(
            "Socket path does not exist: {}",
            socket_path
        )));
    }

    // Connect to the instance
    let mut client = AimxClient::connect(socket_path).await?;

    // List records
    let records = client.list_records().await?;

    debug!("✅ Found {} record(s)", records.len());

    // Convert to JSON
    let records_json: Vec<_> = records
        .into_iter()
        .map(|r| {
            json!({
                "name": r.name,
                "type_id": r.type_id,
                "buffer_type": r.buffer_type,
                "buffer_capacity": r.buffer_capacity,
                "producer_count": r.producer_count,
                "consumer_count": r.consumer_count,
                "writable": r.writable,
                "created_at": r.created_at,
                "last_update": r.last_update,
                "connector_count": r.connector_count,
            })
        })
        .collect();

    let content_json = json!({
        "socket_path": socket_path,
        "records": records_json,
        "count": records_json.len(),
    });

    let uri = format!(
        "{}{}{}",
        RECORDS_URI_PREFIX, socket_path, RECORDS_URI_SUFFIX
    );

    let content = ResourceContent {
        uri,
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

    #[test]
    fn test_is_records_uri() {
        assert!(is_records_uri("aimdb:///tmp/test.sock/records"));
        assert!(is_records_uri("aimdb:///var/run/aimdb/server.sock/records"));
        assert!(!is_records_uri("aimdb://instances"));
        assert!(!is_records_uri("aimdb:///tmp/test.sock"));
        assert!(!is_records_uri("other://something"));
    }

    #[test]
    fn test_extract_socket_path() {
        assert_eq!(
            extract_socket_path("aimdb:///tmp/test.sock/records"),
            Some("/tmp/test.sock".to_string())
        );
        assert_eq!(
            extract_socket_path("aimdb:///var/run/aimdb/server.sock/records"),
            Some("/var/run/aimdb/server.sock".to_string())
        );
        assert_eq!(extract_socket_path("aimdb://instances"), None);
        assert_eq!(extract_socket_path("invalid"), None);
    }

    #[test]
    fn test_records_resource_for_socket() {
        let resource = records_resource_for_socket("/tmp/test.sock");
        assert_eq!(resource.uri, "aimdb:///tmp/test.sock/records");
        assert_eq!(resource.name, "Records: /tmp/test.sock");
        assert!(resource.description.is_some());
        assert_eq!(resource.mime_type, Some("application/json".to_string()));
    }

    #[tokio::test]
    async fn test_list_records_resources() {
        // This test will succeed if instances are found, or fail gracefully if not
        let result = list_records_resources().await;

        match result {
            Ok(resources) => {
                // If instances found, should have at least one resource
                assert!(!resources.is_empty());
                for resource in resources {
                    assert!(resource.uri.starts_with("aimdb://"));
                    assert!(resource.uri.ends_with("/records"));
                }
            }
            Err(err) => {
                // Should fail with "No running AimDB instances found"
                assert!(err.message().contains("No running AimDB instances"));
            }
        }
    }

    #[tokio::test]
    async fn test_read_records_resource_invalid_socket() {
        let result = read_records_resource("/tmp/nonexistent.sock").await;
        assert!(result.is_err());
    }
}
