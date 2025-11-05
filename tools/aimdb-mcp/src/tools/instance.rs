//! Instance-related tools (discover_instances, get_instance_info)

use crate::error::McpResult;
use aimdb_client::{self, connection::AimxClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

/// Result from discover_instances tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredInstance {
    /// Unix socket path
    pub socket_path: String,
    /// Server version
    pub server_version: String,
    /// Protocol version
    pub protocol_version: String,
    /// Permissions granted to the client
    pub permissions: Vec<String>,
    /// Names of writable records
    pub writable_records: Vec<String>,
    /// Maximum subscriptions allowed (if any)
    pub max_subscriptions: Option<usize>,
    /// Whether authentication is required
    pub authenticated: bool,
}

/// Discover all running AimDB instances
///
/// Scans /tmp/*.sock and /var/run/aimdb/*.sock for AimDB instances.
/// Connects to each and retrieves instance information.
pub async fn discover_instances(_args: Option<Value>) -> McpResult<Value> {
    debug!("🔍 Discovering AimDB instances...");

    // Use aimdb_client discovery
    let instances = aimdb_client::discover_instances().await?;

    debug!("✅ Found {} instance(s)", instances.len());

    // Convert to our result format
    let discovered: Vec<DiscoveredInstance> = instances
        .into_iter()
        .map(|info| DiscoveredInstance {
            socket_path: info.socket_path.display().to_string(),
            server_version: info.server_version,
            protocol_version: info.protocol_version,
            permissions: info.permissions,
            writable_records: info.writable_records,
            max_subscriptions: info.max_subscriptions,
            authenticated: info.authenticated,
        })
        .collect();

    // Convert to JSON
    let result = serde_json::to_value(discovered)?;
    Ok(result)
}

/// Parameters for get_instance_info tool
#[derive(Debug, Deserialize)]
struct GetInstanceInfoParams {
    socket_path: String,
}

/// Result from get_instance_info tool
#[derive(Debug, Serialize)]
pub struct InstanceInfoResult {
    /// Unix socket path
    pub socket_path: String,
    /// Server version
    pub server_version: String,
    /// Protocol version
    pub protocol_version: String,
    /// Permissions granted to the client
    pub permissions: Vec<String>,
    /// Names of writable records
    pub writable_records: Vec<String>,
    /// Maximum subscriptions allowed (if any)
    pub max_subscriptions: Option<usize>,
    /// Whether authentication is required
    pub authenticated: bool,
}

/// Get detailed information about a specific AimDB instance
///
/// Connects to the instance and retrieves server metadata from the welcome message.
pub async fn get_instance_info(args: Option<Value>) -> McpResult<Value> {
    let params: GetInstanceInfoParams = match args {
        Some(value) => serde_json::from_value(value)?,
        None => {
            return Err(crate::error::McpError::InvalidParams(
                "Missing socket_path".into(),
            ))
        }
    };

    debug!("🔍 Getting instance info for: {}", params.socket_path);

    // Get or create connection from pool (if available)
    let client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&params.socket_path).await?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxClient::connect(&params.socket_path).await?
    };

    // Get server info from the welcome message
    let server_info = client.server_info();

    // Convert to result format
    let result = InstanceInfoResult {
        socket_path: params.socket_path.clone(),
        server_version: server_info.server.clone(),
        protocol_version: server_info.version.clone(),
        permissions: server_info.permissions.clone(),
        writable_records: server_info.writable_records.clone(),
        max_subscriptions: server_info.max_subscriptions,
        authenticated: server_info.authenticated.unwrap_or(false),
    };

    debug!("✅ Retrieved instance info: {:?}", result);

    // Convert to JSON
    Ok(serde_json::to_value(result)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discover_instances() {
        // Discovery should either succeed (if instances exist) or fail gracefully
        let result = discover_instances(None).await;

        match result {
            Ok(value) => {
                // If instances found, should be a valid JSON array
                assert!(value.is_array());
                println!("Found {} instance(s)", value.as_array().unwrap().len());
            }
            Err(err) => {
                // If no instances, should have appropriate error message
                assert!(err.message().contains("No running AimDB instances"));
            }
        }
    }

    #[tokio::test]
    async fn test_get_instance_info_missing_params() {
        let result = get_instance_info(None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message().contains("Missing socket_path"));
    }

    #[tokio::test]
    async fn test_get_instance_info_invalid_socket() {
        let params = serde_json::json!({
            "socket_path": "/tmp/nonexistent.sock"
        });
        let result = get_instance_info(Some(params)).await;
        assert!(result.is_err());
    }
}
