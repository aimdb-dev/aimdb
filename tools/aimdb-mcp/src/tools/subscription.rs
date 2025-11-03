//! Subscription management tools for real-time record monitoring
//!
//! Implements subscribe_record, unsubscribe_record, and list_subscriptions tools.

use crate::error::{McpError, McpResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// Parameters for subscribe_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRecordParams {
    /// Unix socket path to the AimDB instance
    pub socket_path: String,
    /// Name of the record to subscribe to
    pub record_name: String,
}

/// Parameters for unsubscribe_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeRecordParams {
    /// Subscription ID to cancel
    pub subscription_id: String,
}

/// Subscribe to record value changes
///
/// Subscribes to a specific record in an AimDB instance. When the record value
/// changes, the server will send notifications/resources/updated messages to the client.
///
/// # Arguments
///
/// * `args` - JSON object with socket_path and record_name
///
/// # Returns
///
/// JSON object with subscription_id
///
/// # Example
///
/// ```json
/// {
///   "socket_path": "/tmp/aimdb-demo.sock",
///   "record_name": "server::Temperature"
/// }
/// ```
///
/// Returns:
/// ```json
/// {
///   "subscription_id": "sub_1234567890",
///   "socket_path": "/tmp/aimdb-demo.sock",
///   "record_name": "server::Temperature",
///   "created_at": 1730649600000
/// }
/// ```
pub async fn subscribe_record(args: Option<Value>) -> McpResult<Value> {
    let params: SubscribeRecordParams = serde_json::from_value(
        args.ok_or_else(|| McpError::InvalidParams("Missing arguments".to_string()))?,
    )
    .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!(
        "🔔 Subscribing to record: {} at {}",
        params.record_name, params.socket_path
    );

    // Get subscription manager
    let manager = crate::tools::subscription_manager()
        .ok_or_else(|| McpError::Internal("Subscription manager not initialized".to_string()))?;

    // Create subscription with queue size of 10
    let subscription_id = manager
        .subscribe(
            std::path::PathBuf::from(&params.socket_path),
            params.record_name.clone(),
            10, // queue_size
        )
        .await?;

    // Get subscription info
    let info = manager
        .get_subscription(&subscription_id)
        .await
        .ok_or_else(|| McpError::Internal("Subscription vanished after creation".to_string()))?;

    Ok(json!({
        "subscription_id": info.subscription_id,
        "socket_path": info.socket_path.display().to_string(),
        "record_name": info.record_name,
        "aimx_subscription_id": info.aimx_subscription_id,
        "created_at": info.created_at
    }))
}

/// Unsubscribe from record value changes
///
/// Cancels an active subscription and stops receiving notifications.
///
/// # Arguments
///
/// * `args` - JSON object with subscription_id
///
/// # Returns
///
/// JSON object confirming unsubscription
///
/// # Example
///
/// ```json
/// {
///   "subscription_id": "sub_1234567890"
/// }
/// ```
///
/// Returns:
/// ```json
/// {
///   "success": true,
///   "subscription_id": "sub_1234567890"
/// }
/// ```
pub async fn unsubscribe_record(args: Option<Value>) -> McpResult<Value> {
    let params: UnsubscribeRecordParams = serde_json::from_value(
        args.ok_or_else(|| McpError::InvalidParams("Missing arguments".to_string()))?,
    )
    .map_err(|e| McpError::InvalidParams(format!("Invalid parameters: {}", e)))?;

    debug!("🔕 Unsubscribing: {}", params.subscription_id);

    // Get subscription manager
    let manager = crate::tools::subscription_manager()
        .ok_or_else(|| McpError::Internal("Subscription manager not initialized".to_string()))?;

    // Unsubscribe
    manager.unsubscribe(&params.subscription_id).await?;

    Ok(json!({
        "success": true,
        "subscription_id": params.subscription_id
    }))
}

/// List all active subscriptions
///
/// Returns information about all currently active subscriptions.
///
/// # Arguments
///
/// * `args` - No arguments required
///
/// # Returns
///
/// JSON object with subscriptions array
///
/// # Example
///
/// Returns:
/// ```json
/// {
///   "count": 2,
///   "subscriptions": [
///     {
///       "subscription_id": "sub_1234567890",
///       "socket_path": "/tmp/aimdb-demo.sock",
///       "record_name": "server::Temperature",
///       "aimx_subscription_id": "aimx_sub_abc123",
///       "created_at": 1730649600000
///     },
///     {
///       "subscription_id": "sub_1234567891",
///       "socket_path": "/tmp/aimdb-demo.sock",
///       "record_name": "server::AppSettings",
///       "aimx_subscription_id": "aimx_sub_def456",
///       "created_at": 1730649601000
///     }
///   ]
/// }
/// ```
pub async fn list_subscriptions(_args: Option<Value>) -> McpResult<Value> {
    debug!("📋 Listing active subscriptions");

    // Get subscription manager
    let manager = crate::tools::subscription_manager()
        .ok_or_else(|| McpError::Internal("Subscription manager not initialized".to_string()))?;

    // Get all subscriptions
    let subscriptions = manager.list_subscriptions().await;

    let subs_json: Vec<Value> = subscriptions
        .iter()
        .map(|info| {
            json!({
                "subscription_id": info.subscription_id,
                "socket_path": info.socket_path.display().to_string(),
                "record_name": info.record_name,
                "aimx_subscription_id": info.aimx_subscription_id,
                "created_at": info.created_at
            })
        })
        .collect();

    Ok(json!({
        "count": subs_json.len(),
        "subscriptions": subs_json
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscribe_record_missing_args() {
        let result = subscribe_record(None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn test_unsubscribe_record_missing_args() {
        let result = unsubscribe_record(None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    // Note: Full integration tests with subscription manager are in examples/
    // These unit tests only validate parameter parsing
}
