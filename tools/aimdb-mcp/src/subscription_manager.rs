//! Subscription lifecycle management
//!
//! Manages active subscriptions, handles connection lifecycle, and forwards
//! events from AimDB instances as MCP notifications.

use crate::error::{McpError, McpResult};
use crate::protocol::Notification;
use aimdb_client::AimxClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Subscription metadata
#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    /// Unique subscription ID
    pub subscription_id: String,
    /// Socket path to AimDB instance
    pub socket_path: PathBuf,
    /// Record name being monitored
    pub record_name: String,
    /// AimX subscription ID (from server)
    pub aimx_subscription_id: String,
    /// Creation timestamp (Unix millis)
    pub created_at: i64,
}

/// Manager for handling AimDB subscriptions and forwarding events
pub struct SubscriptionManager {
    /// Active subscriptions
    subscriptions: Arc<RwLock<HashMap<String, SubscriptionInfo>>>,
    /// Notification sender (sends to MCP client)
    notification_tx: mpsc::UnboundedSender<Notification>,
    /// Background task handles
    tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl SubscriptionManager {
    /// Create a new subscription manager
    pub fn new(notification_tx: mpsc::UnboundedSender<Notification>) -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            notification_tx,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Subscribe to a record
    ///
    /// Creates a new subscription and spawns a background task to receive events.
    pub async fn subscribe(
        &self,
        socket_path: PathBuf,
        record_name: String,
        queue_size: usize,
    ) -> McpResult<String> {
        // Generate unique subscription ID
        let subscription_id = format!("sub_{}", chrono::Utc::now().timestamp_millis());

        debug!(
            "📡 Creating subscription {} for {} at {}",
            subscription_id,
            record_name,
            socket_path.display()
        );

        // Connect to AimDB instance
        let mut client = AimxClient::connect(&socket_path).await.map_err(|e| {
            McpError::Internal(format!(
                "Failed to connect to {}: {}",
                socket_path.display(),
                e
            ))
        })?;

        // Subscribe to record
        let aimx_subscription_id =
            client
                .subscribe(&record_name, queue_size)
                .await
                .map_err(|e| {
                    McpError::Internal(format!("Failed to subscribe to {}: {}", record_name, e))
                })?;

        debug!(
            "✅ AimX subscription created: {} -> {}",
            subscription_id, aimx_subscription_id
        );

        // Create subscription info
        let info = SubscriptionInfo {
            subscription_id: subscription_id.clone(),
            socket_path: socket_path.clone(),
            record_name: record_name.clone(),
            aimx_subscription_id: aimx_subscription_id.clone(),
            created_at: chrono::Utc::now().timestamp_millis(),
        };

        // Store subscription
        self.subscriptions
            .write()
            .await
            .insert(subscription_id.clone(), info.clone());

        // Spawn background task to receive events
        let handle = self.spawn_event_listener(
            subscription_id.clone(),
            client,
            record_name.clone(),
            socket_path.clone(),
        );

        // Store task handle
        self.tasks
            .lock()
            .await
            .insert(subscription_id.clone(), handle);

        info!(
            "🎉 Subscription {} created for {} at {}",
            subscription_id,
            record_name,
            socket_path.display()
        );

        Ok(subscription_id)
    }

    /// Unsubscribe from a record
    pub async fn unsubscribe(&self, subscription_id: &str) -> McpResult<()> {
        debug!("🔕 Unsubscribing: {}", subscription_id);

        // Remove subscription info
        let info = self
            .subscriptions
            .write()
            .await
            .remove(subscription_id)
            .ok_or_else(|| {
                McpError::InvalidParams(format!("Unknown subscription: {}", subscription_id))
            })?;

        // Cancel background task
        if let Some(handle) = self.tasks.lock().await.remove(subscription_id) {
            handle.abort();
            debug!("✅ Cancelled event listener task for {}", subscription_id);
        }

        info!(
            "✅ Unsubscribed {} from {} at {}",
            subscription_id,
            info.record_name,
            info.socket_path.display()
        );

        Ok(())
    }

    /// List all active subscriptions
    pub async fn list_subscriptions(&self) -> Vec<SubscriptionInfo> {
        self.subscriptions.read().await.values().cloned().collect()
    }

    /// Get subscription info
    pub async fn get_subscription(&self, subscription_id: &str) -> Option<SubscriptionInfo> {
        self.subscriptions
            .read()
            .await
            .get(subscription_id)
            .cloned()
    }

    /// Cleanup all subscriptions (called on shutdown)
    pub async fn cleanup(&self) {
        info!("🧹 Cleaning up all subscriptions");

        // Abort all background tasks
        let mut tasks = self.tasks.lock().await;
        for (id, handle) in tasks.drain() {
            handle.abort();
            debug!("Aborted task for subscription {}", id);
        }

        // Clear subscriptions
        self.subscriptions.write().await.clear();

        info!("✅ All subscriptions cleaned up");
    }

    /// Spawn a background task to listen for events
    fn spawn_event_listener(
        &self,
        subscription_id: String,
        mut client: AimxClient,
        record_name: String,
        socket_path: PathBuf,
    ) -> JoinHandle<()> {
        let notification_tx = self.notification_tx.clone();
        let subscriptions = self.subscriptions.clone();
        let tasks = self.tasks.clone();

        tokio::spawn(async move {
            debug!(
                "🎧 Event listener started for {} ({})",
                subscription_id, record_name
            );

            loop {
                // Check if subscription still exists (may have been cancelled)
                if !subscriptions.read().await.contains_key(&subscription_id) {
                    debug!(
                        "Subscription {} no longer exists, stopping listener",
                        subscription_id
                    );
                    break;
                }

                // Receive next event
                match client.receive_event().await {
                    Ok(event) => {
                        debug!(
                            "📬 Received event for {}: sequence={}",
                            subscription_id, event.sequence
                        );

                        // Forward as MCP notification
                        let notification = Notification::record_changed(
                            &subscription_id,
                            &record_name,
                            event.data,
                        );

                        if let Err(e) = notification_tx.send(notification) {
                            error!("Failed to send notification for {}: {}", subscription_id, e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            "⚠️  Error receiving event for {} at {}: {}",
                            subscription_id,
                            socket_path.display(),
                            e
                        );

                        // Send error notification
                        let error_notification = Notification::subscription_error(
                            &subscription_id,
                            &format!("Event receive error: {}", e),
                        );

                        if let Err(e) = notification_tx.send(error_notification) {
                            error!("Failed to send error notification: {}", e);
                        }

                        // Remove subscription on error
                        subscriptions.write().await.remove(&subscription_id);
                        tasks.lock().await.remove(&subscription_id);
                        break;
                    }
                }
            }

            debug!(
                "🔚 Event listener stopped for {} ({})",
                subscription_id, record_name
            );
        })
    }
}

impl Drop for SubscriptionManager {
    fn drop(&mut self) {
        // Note: We can't use async in Drop, so subscriptions will be
        // cleaned up when tasks are aborted naturally
        debug!("SubscriptionManager dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscription_manager_creation() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let manager = SubscriptionManager::new(tx);

        let subs = manager.list_subscriptions().await;
        assert_eq!(subs.len(), 0);
    }

    #[tokio::test]
    async fn test_subscription_info_clone() {
        let info = SubscriptionInfo {
            subscription_id: "sub_123".to_string(),
            socket_path: PathBuf::from("/tmp/test.sock"),
            record_name: "test::Record".to_string(),
            aimx_subscription_id: "aimx_sub_456".to_string(),
            created_at: 1234567890,
        };

        let cloned = info.clone();
        assert_eq!(cloned.subscription_id, info.subscription_id);
        assert_eq!(cloned.record_name, info.record_name);
    }

    #[tokio::test]
    async fn test_unsubscribe_nonexistent() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let manager = SubscriptionManager::new(tx);

        let result = manager.unsubscribe("nonexistent").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }
}
