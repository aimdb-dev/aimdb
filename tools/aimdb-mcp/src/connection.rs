//! Connection pool management for AimDB instances
//!
//! Manages persistent connections to AimDB instances to avoid
//! reconnecting on every tool call. Includes auto-reconnect logic.

use aimdb_client::AimxConnection;
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
#[derive(Clone)]
pub struct ConnectionPool {
    /// Track which connections we've attempted (for logging/metrics)
    connections: Arc<Mutex<HashMap<String, ConnectionEntry>>>,
    /// Persistent drain clients — kept alive so drain readers accumulate values
    /// Key: endpoint, Value: shared AimxConnection
    drain_clients: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<AimxConnection>>>>>,
}

impl std::fmt::Debug for ConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionPool")
            .field("connections", &"<...>")
            .field("drain_clients", &"<...>")
            .finish()
    }
}

impl ConnectionPool {
    /// Create a new connection pool
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            drain_clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create a connection to an AimDB instance
    ///
    /// Note: Since AimxConnection doesn't implement Clone, we create a fresh
    /// connection each time. The pool tracks connection metadata for
    /// monitoring and future optimization (e.g., persistent connections
    /// via Arc<Mutex<AimxConnection>> if AimxConnection becomes Sync).
    pub async fn get_connection(&self, endpoint: &str) -> Result<AimxConnection, ClientError> {
        let mut pool = self.connections.lock().await;

        // Update or insert connection metadata
        let now = std::time::Instant::now();

        if let Some(entry) = pool.get_mut(endpoint) {
            debug!(
                "♻️  Connection metadata exists for {}, reconnecting",
                endpoint
            );
            entry.last_used = now;
        } else {
            debug!("🔌 First connection to {}", endpoint);
            pool.insert(endpoint.to_string(), ConnectionEntry { last_used: now });
        }

        // Always create a new connection (until AimxConnection supports cloning/sharing)
        drop(pool); // Release lock before async operation
        AimxConnection::connect(endpoint).await
    }

    /// Remove a connection from the pool (called when operations fail)
    pub async fn invalidate_connection(&self, endpoint: &str) {
        let mut pool = self.connections.lock().await;
        if pool.remove(endpoint).is_some() {
            debug!("❌ Invalidated connection metadata for {}", endpoint);
        }
    }

    /// Get or create a persistent drain client for a socket path.
    ///
    /// Drain clients are kept alive across calls so the server-side drain
    /// reader accumulates values between invocations. The first drain call
    /// on a new connection is a cold start (returns empty); subsequent calls
    /// return all values accumulated since the previous drain.
    pub async fn get_drain_client(
        &self,
        endpoint: &str,
    ) -> Result<Arc<tokio::sync::Mutex<AimxConnection>>, ClientError> {
        let drain_map = self.drain_clients.lock().await;

        if let Some(client) = drain_map.get(endpoint) {
            debug!("♻️  Reusing persistent drain client for {}", endpoint);
            return Ok(Arc::clone(client));
        }

        debug!("🔌 Creating persistent drain client for {}", endpoint);

        // Drop lock before async connect
        drop(drain_map);

        let client = AimxConnection::connect(endpoint).await?;
        let shared = Arc::new(tokio::sync::Mutex::new(client));

        let mut drain_map = self.drain_clients.lock().await;
        // Double-check: another task may have inserted while we were connecting
        if let Some(existing) = drain_map.get(endpoint) {
            return Ok(Arc::clone(existing));
        }
        drain_map.insert(endpoint.to_string(), Arc::clone(&shared));
        Ok(shared)
    }

    /// Invalidate (remove) a persistent drain client, e.g. after connection error
    pub async fn invalidate_drain_client(&self, endpoint: &str) {
        let mut drain_map = self.drain_clients.lock().await;
        if drain_map.remove(endpoint).is_some() {
            debug!("❌ Invalidated drain client for {}", endpoint);
        }
    }

    /// Clear all connections in the pool
    pub async fn clear(&self) {
        let mut pool = self.connections.lock().await;
        pool.clear();
        let mut drain_map = self.drain_clients.lock().await;
        drain_map.clear();
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
