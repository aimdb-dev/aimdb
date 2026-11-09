//! Persistent AimX clients, one per endpoint.
//!
//! Every tool and resource reaches an AimDB instance through this pool, so an
//! endpoint only ever has one connection.

use aimdb_client::AimxConnection;
use aimdb_client::ClientError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// A persistent client. Lock it for the duration of one call.
pub type SharedClient = Arc<Mutex<AimxConnection>>;

/// An endpoint's client, or `None` before the first connect. Its lock is held
/// across `connect` so an endpoint only ever has one attempt in flight.
type ClientSlot = Arc<Mutex<Option<SharedClient>>>;

/// Pool of persistent AimDB clients
#[derive(Clone)]
pub struct ConnectionPool {
    /// Key: endpoint
    clients: Arc<Mutex<HashMap<String, ClientSlot>>>,
}

impl std::fmt::Debug for ConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionPool")
            .field("clients", &"<...>")
            .finish()
    }
}

impl ConnectionPool {
    /// Create a new connection pool
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create the persistent client for an endpoint.
    ///
    /// One connection per endpoint, kept across calls and shared by every tool;
    /// callers take its lock for each call. Keeping it open matters twice: the
    /// server's drain cursor lives in the session, so values accumulate between
    /// drains, and on serial a second connection either fails (the tty is open)
    /// or sends a new `hello` that resets the board's session and discards that
    /// cursor. For the same reason concurrent first calls wait for one connect
    /// instead of racing their own.
    ///
    /// The client is replaced only once its engine has stopped
    /// ([`AimxConnection::is_closed`]), never because a call returned an error:
    /// `not_found`, `denied` and even `internal` are replies on a healthy
    /// session. A client busy with a call is returned as is; if its engine has
    /// stopped, the next call replaces it.
    pub async fn get_client(&self, endpoint: &str) -> Result<SharedClient, ClientError> {
        let slot = {
            let mut clients = self.clients.lock().await;
            Arc::clone(clients.entry(endpoint.to_string()).or_default())
        };
        let mut slot = slot.lock().await;

        if let Some(client) = slot.as_ref() {
            if !client.try_lock().is_ok_and(|c| c.is_closed()) {
                debug!("♻️  Reusing persistent client for {}", endpoint);
                return Ok(Arc::clone(client));
            }
            debug!(
                "🔁 Persistent client for {} has stopped, reconnecting",
                endpoint
            );
            *slot = None;
        }

        debug!("🔌 Creating persistent client for {}", endpoint);
        let client = Arc::new(Mutex::new(AimxConnection::connect(endpoint).await?));
        *slot = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Forget every client
    pub async fn clear(&self) {
        self.clients.lock().await.clear();
        debug!("🧹 Cleared connection pool");
    }

    /// Number of endpoints the pool has been asked to connect to
    pub async fn connection_count(&self) -> usize {
        self.clients.lock().await.len()
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
}
