//! Instance-related tools (discover_instances, get_instance_info)

use crate::error::McpResult;
use aimdb_client::protocol::InstanceInfoReply;
use aimdb_client::{self, AimxConnection};
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
    /// Instance name given at startup (protocol 3.1); `null` when unset
    #[serde(default)]
    pub name: Option<String>,
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
            socket_path: info.endpoint.display().to_string(),
            server_version: info.server_version,
            protocol_version: info.protocol_version,
            permissions: info.permissions,
            writable_records: info.writable_records,
            max_subscriptions: info.max_subscriptions,
            authenticated: info.authenticated,
            name: info.name,
        })
        .collect();

    // Convert to JSON
    let result = serde_json::to_value(discovered)?;
    Ok(result)
}

/// Parameters for get_instance_info tool
#[derive(Debug, Deserialize)]
struct GetInstanceInfoParams {
    /// Endpoint URL or socket path of the AimDB instance (falls back to AIMDB_CONNECT env)
    endpoint: Option<String>,
}

/// Result from get_instance_info tool
#[derive(Debug, Serialize)]
pub struct InstanceInfoResult {
    /// Endpoint the instance was reached at
    pub endpoint: String,
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
    /// `aimdb_version`, `instance` (name, UID) and `build` from
    /// `instance.info` (protocol 3.1); absent from a 3.0 server
    #[serde(flatten)]
    pub instance_info: Option<InstanceInfoReply>,
}

/// Get detailed information about a specific AimDB instance
///
/// Connects to the instance and retrieves server metadata from the welcome
/// message, plus its identity and build info from `instance.info`.
pub async fn get_instance_info(args: Option<Value>) -> McpResult<Value> {
    // Omitted args (or `{}`) is not an error here — it just means "no explicit
    // endpoint", so resolution can still fall back to `--connect` / AIMDB_CONNECT.
    let explicit = match args {
        Some(value) => {
            let params: GetInstanceInfoParams = serde_json::from_value(value).map_err(|e| {
                crate::error::McpError::InvalidParams(format!("get_instance_info: {e}"))
            })?;
            params.endpoint
        }
        None => None,
    };
    let endpoint = super::resolve_endpoint(explicit)?;

    debug!("🔍 Getting instance info for: {}", endpoint);

    // Get or create connection from pool (if available)
    let client = if let Some(pool) = super::connection_pool() {
        pool.get_connection(&endpoint).await?
    } else {
        // Fallback to direct connection if pool not initialized
        AimxConnection::connect(&endpoint).await?
    };

    // Get server info from the welcome message
    let server_info = client.server_info();

    // Convert to result format
    let result = InstanceInfoResult {
        endpoint: endpoint.clone(),
        server_version: server_info.server.clone(),
        protocol_version: server_info.version.clone(),
        permissions: server_info.permissions.clone(),
        writable_records: server_info.writable_records.clone(),
        max_subscriptions: server_info.max_subscriptions,
        authenticated: server_info.authenticated.unwrap_or(false),
        instance_info: client.instance_info().await?,
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

    fn info_result(instance_info: Option<InstanceInfoReply>) -> Value {
        serde_json::to_value(InstanceInfoResult {
            endpoint: "/tmp/aimdb.sock".to_string(),
            server_version: "aimdb".to_string(),
            protocol_version: "3.1".to_string(),
            permissions: vec!["read".to_string()],
            writable_records: vec![],
            max_subscriptions: Some(8),
            authenticated: false,
            instance_info,
        })
        .unwrap()
    }

    #[test]
    fn instance_info_fields_are_flattened_in() {
        let reply: InstanceInfoReply = serde_json::from_value(serde_json::json!({
            "aimdb_version": "2.1.0",
            "instance": { "name": "gateway-3", "uid": "00ff" },
            "build": {
                "format": 1,
                "info": { "schema": 1, "name": "app" },
                "post_link": { "state": "erased" },
            },
        }))
        .unwrap();
        let value = info_result(Some(reply));
        assert_eq!(value["endpoint"], "/tmp/aimdb.sock");
        assert_eq!(value["aimdb_version"], "2.1.0");
        assert_eq!(value["instance"]["uid"], "00ff");
        assert_eq!(value["build"]["info"]["name"], "app");
        assert_eq!(value["build"]["post_link"]["state"], "erased");
    }

    #[test]
    fn instance_info_fields_are_absent_from_a_3_0_server() {
        let value = info_result(None);
        for key in ["aimdb_version", "instance", "build"] {
            assert!(value.get(key).is_none(), "{key}: {value}");
        }
        assert_eq!(value["protocol_version"], "3.1");
    }

    #[test]
    fn discovered_instance_carries_the_name() {
        let value = serde_json::to_value(DiscoveredInstance {
            socket_path: "/tmp/aimdb.sock".to_string(),
            server_version: "aimdb".to_string(),
            protocol_version: "3.1".to_string(),
            permissions: vec![],
            writable_records: vec![],
            max_subscriptions: None,
            authenticated: false,
            name: Some("gateway-3".to_string()),
        })
        .unwrap();
        assert_eq!(value["name"], "gateway-3");
    }

    #[tokio::test]
    async fn test_get_instance_info_missing_params() {
        let result = get_instance_info(None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message().contains("Missing endpoint"));
    }

    #[tokio::test]
    async fn test_get_instance_info_invalid_socket() {
        let params = serde_json::json!({
            "endpoint": "/tmp/nonexistent.sock"
        });
        let result = get_instance_info(Some(params)).await;
        assert!(result.is_err());
    }
}
