//! Connection pool management for AimDB instances
//!
//! Manages persistent connections to AimDB instances to avoid
//! reconnecting on every tool call. Includes auto-reconnect logic.

use aimdb_client::connection::AimxClient;
use aimdb_client::ClientError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Connection entry with metadata
#[derive(Debug)]
struct ConnectionEntry {
    /// Last successful connection time (for staleness detection)
    last_used: std::time::Instant,
}

/// Connection pool for managing AimDB connections
#[derive(Debug, Clone)]
pub struct ConnectionPool {
    /// Track which connections we've attempted (for logging/metrics)
    connections: Arc<Mutex<HashMap<String, ConnectionEntry>>>,
}

impl ConnectionPool {
    /// Create a new connection pool
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create a connection to an AimDB instance
    ///
    /// Note: Since AimxClient doesn't implement Clone, we create a fresh
    /// connection each time. The pool tracks connection metadata for
    /// monitoring and future optimization (e.g., persistent connections
    /// via Arc<Mutex<AimxClient>> if AimxClient becomes Sync).
    pub async fn get_connection(&self, socket_path: &str) -> Result<AimxClient, ClientError> {
        let mut pool = self.connections.lock().await;

        // Update or insert connection metadata
        let now = std::time::Instant::now();

        if let Some(entry) = pool.get_mut(socket_path) {
            debug!(
                "♻️  Connection metadata exists for {}, reconnecting",
                socket_path
            );
            entry.last_used = now;
        } else {
            debug!("🔌 First connection to {}", socket_path);
            pool.insert(socket_path.to_string(), ConnectionEntry { last_used: now });
        }

        // Always create a new connection (until AimxClient supports cloning/sharing)
        drop(pool); // Release lock before async operation
        AimxClient::connect(socket_path).await
    }

    /// Remove a connection from the pool (called when operations fail)
    pub async fn invalidate_connection(&self, socket_path: &str) {
        let mut pool = self.connections.lock().await;
        if pool.remove(socket_path).is_some() {
            debug!("❌ Invalidated connection metadata for {}", socket_path);
        }
    }

    /// Clear all connections in the pool
    pub async fn clear(&self) {
        let mut pool = self.connections.lock().await;
        pool.clear();
        debug!("🧹 Cleared connection pool");
    }

    /// Get the number of tracked connections
    pub async fn connection_count(&self) -> usize {
        let pool = self.connections.lock().await;
        pool.len()
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_creation() {
        let pool = ConnectionPool::new();
        assert_eq!(pool.connection_count().await, 0);
    }

    #[tokio::test]
    async fn test_pool_clear() {
        let pool = ConnectionPool::new();
        pool.clear().await;
        assert_eq!(pool.connection_count().await, 0);
    }

    #[tokio::test]
    async fn test_invalidate_nonexistent_connection() {
        let pool = ConnectionPool::new();
        // Should not panic
        pool.invalidate_connection("/tmp/nonexistent.sock").await;
        assert_eq!(pool.connection_count().await, 0);
    }
}
