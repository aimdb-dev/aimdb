//! AimDB Instance Discovery
//!
//! Scans known directories for running AimDB instances.

use crate::engine::AimxConnection;
use crate::error::{ClientError, ClientResult};
use crate::protocol::WelcomeMessage;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Known directories where AimDB sockets might be located
const SOCKET_SEARCH_DIRS: &[&str] = &["/tmp", "/var/run/aimdb"];

/// Information about a discovered AimDB instance
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub endpoint: PathBuf,
    pub server_version: String,
    pub protocol_version: String,
    pub permissions: Vec<String>,
    pub writable_records: Vec<String>,
    pub max_subscriptions: Option<usize>,
    pub authenticated: bool,
}

impl From<(PathBuf, WelcomeMessage)> for InstanceInfo {
    fn from((endpoint, welcome): (PathBuf, WelcomeMessage)) -> Self {
        Self {
            endpoint,
            server_version: welcome.server,
            protocol_version: welcome.version,
            permissions: welcome.permissions,
            writable_records: welcome.writable_records,
            max_subscriptions: welcome.max_subscriptions,
            authenticated: welcome.authenticated.unwrap_or(false),
        }
    }
}

/// Discover all running AimDB instances
pub async fn discover_instances() -> ClientResult<Vec<InstanceInfo>> {
    let mut instances = Vec::new();

    for dir_path in SOCKET_SEARCH_DIRS {
        if let Ok(entries) = tokio::fs::read_dir(dir_path).await {
            instances.extend(scan_directory(entries).await);
        }
    }

    if instances.is_empty() {
        return Err(ClientError::NoInstancesFound);
    }

    Ok(instances)
}

/// Scan a directory for AimDB socket files
async fn scan_directory(mut entries: tokio::fs::ReadDir) -> Vec<InstanceInfo> {
    let mut instances = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();

        // Check if it's a socket file (ends with .sock)
        if path.extension().and_then(|s| s.to_str()) == Some("sock") {
            // Try to connect and get instance info
            if let Ok(info) = probe_instance(&path).await {
                instances.push(info);
            }
        }
    }

    instances
}

/// Try to connect to a socket and get instance information
async fn probe_instance(socket_path: &Path) -> ClientResult<InstanceInfo> {
    // `connect_with_timeout` bounds the whole handshake (dial + hello), so a stale
    // socket whose peer accepts but never replies fails fast instead of hanging —
    // no need to wrap a second timeout around `connect`.
    let connect_timeout = Duration::from_millis(500);
    // A discovered socket path is dialed as the bare-path (`unix://`) shorthand.
    let endpoint = socket_path.to_string_lossy();
    let client = AimxConnection::connect_with_timeout(&endpoint, connect_timeout).await?;

    let welcome = client.server_info().clone();

    Ok(InstanceInfo::from((socket_path.to_path_buf(), welcome)))
}

/// Find a specific instance by socket path or name
pub async fn find_instance(socket_hint: Option<&str>) -> ClientResult<InstanceInfo> {
    // If socket path provided, try that directly
    if let Some(socket_path) = socket_hint {
        let path = PathBuf::from(socket_path);
        if path.exists() {
            return probe_instance(&path).await;
        } else {
            return Err(ClientError::connection_failed(
                socket_path.to_string(),
                "socket file does not exist",
            ));
        }
    }

    // Otherwise, discover all and return the first one
    let instances = discover_instances().await?;

    instances
        .into_iter()
        .next()
        .ok_or(ClientError::NoInstancesFound)
}
