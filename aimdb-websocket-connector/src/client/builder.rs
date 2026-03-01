//! Builder for the WebSocket client connector.
//!
//! [`WsClientConnectorBuilder`] implements [`ConnectorBuilder<R>`] following the
//! same pattern as `MqttConnectorBuilder` and the server-side
//! `WebSocketConnectorBuilder`.
//!
//! # Lifecycle
//!
//! ```text
//! AimDbBuilder::build()
//!   └─ WsClientConnectorBuilder::build(&db)
//!        ├─ db.collect_inbound_routes("ws-client")  → Router
//!        ├─ db.collect_outbound_routes("ws-client")  → outbound tasks
//!        ├─ connect to remote WebSocket server
//!        ├─ spawn receive loop (router dispatch)
//!        ├─ spawn outbound publisher tasks
//!        └─ return Arc<WsClientConnectorImpl>
//! ```

use std::{pin::Pin, sync::Arc, time::Duration};

use aimdb_core::{router::RouterBuilder, ConnectorBuilder};

use super::connector::WsClientConnectorImpl;

// ════════════════════════════════════════════════════════════════════
// Builder
// ════════════════════════════════════════════════════════════════════

/// Builder for the AimDB WebSocket client connector.
///
/// Connects *out* to a remote WebSocket server for direct AimDB-to-AimDB sync.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_websocket_connector::WsClientConnector;
///
/// let connector = WsClientConnector::new("wss://cloud.example.com/ws")
///     .with_auto_reconnect(true)
///     .with_keepalive_ms(30_000)
///     .with_max_offline_queue(256);
/// ```
pub struct WsClientConnectorBuilder {
    /// WebSocket URL to connect to (e.g., `wss://cloud.example.com/ws`).
    url: String,
    /// Re-connect automatically on close (default: true).
    auto_reconnect: bool,
    /// Maximum reconnect attempts before giving up (0 = unlimited, default: 0).
    max_reconnect_attempts: usize,
    /// Keepalive ping interval in milliseconds (default: 30_000).
    keepalive_ms: u64,
    /// Maximum queued writes while disconnected (default: 256).
    max_offline_queue: usize,
    /// Topics to subscribe to on the remote server immediately after connect.
    /// Wildcards supported (e.g., `["sensors/#"]`).
    subscribe_topics: Vec<String>,
    /// Request late-join snapshots on (re)connect (default: true).
    late_join: bool,
}

impl WsClientConnectorBuilder {
    /// Create a new builder targeting the given WebSocket URL.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// WsClientConnector::new("wss://cloud.example.com/ws")
    /// WsClientConnector::new("ws://192.168.1.100:8080/ws")
    /// ```
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auto_reconnect: true,
            max_reconnect_attempts: 0,
            keepalive_ms: 30_000,
            max_offline_queue: 256,
            subscribe_topics: Vec::new(),
            late_join: true,
        }
    }

    /// Enable or disable automatic reconnection on disconnect (default: `true`).
    pub fn with_auto_reconnect(mut self, enabled: bool) -> Self {
        self.auto_reconnect = enabled;
        self
    }

    /// Set maximum reconnect attempts (0 = unlimited, default: 0).
    pub fn with_max_reconnect_attempts(mut self, max: usize) -> Self {
        self.max_reconnect_attempts = max;
        self
    }

    /// Set the keepalive ping interval in milliseconds (default: 30 000).
    ///
    /// Set to 0 to disable keepalive pings.
    pub fn with_keepalive_ms(mut self, ms: u64) -> Self {
        self.keepalive_ms = ms;
        self
    }

    /// Set the maximum number of queued writes while disconnected (default: 256).
    ///
    /// When the queue is full, new writes are silently dropped.
    pub fn with_max_offline_queue(mut self, max: usize) -> Self {
        self.max_offline_queue = max;
        self
    }

    /// Subscribe to these topic patterns on the remote server immediately
    /// after connecting.
    ///
    /// If not set, inbound routes are derived from `link_from("ws-client://…")`
    /// declarations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// WsClientConnector::new("wss://cloud/ws")
    ///     .with_subscribe_topics(["sensors/#", "config/#"])
    /// ```
    pub fn with_subscribe_topics(
        mut self,
        topics: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.subscribe_topics = topics.into_iter().map(Into::into).collect();
        self
    }

    /// Enable or disable late-join snapshot requests on connect (default: `true`).
    pub fn with_late_join(mut self, enabled: bool) -> Self {
        self.late_join = enabled;
        self
    }
}

// ════════════════════════════════════════════════════════════════════
// ConnectorBuilder impl
// ════════════════════════════════════════════════════════════════════

impl<R> ConnectorBuilder<R> for WsClientConnectorBuilder
where
    R: aimdb_executor::Spawn + 'static,
{
    fn scheme(&self) -> &str {
        "ws-client"
    }

    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<
        Box<
            dyn core::future::Future<
                    Output = aimdb_core::DbResult<Arc<dyn aimdb_core::transport::Connector>>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            // ── Inbound routes ──────────────────────────────────────
            let inbound_routes = db.collect_inbound_routes("ws-client");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "WS client: {} inbound routes collected",
                inbound_routes.len()
            );

            let router = Arc::new(RouterBuilder::from_routes(inbound_routes).build());

            // ── Outbound routes ──────────────────────────────────────
            let outbound_routes = db.collect_outbound_routes("ws-client");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "WS client: {} outbound routes collected",
                outbound_routes.len()
            );

            // ── Resolve subscribe topics ─────────────────────────────
            // Merge explicit subscribe_topics with topics derived from inbound routes
            let mut topics: Vec<String> = self.subscribe_topics.clone();
            for resource_id in router.resource_ids() {
                let topic = resource_id.to_string();
                if !topics.contains(&topic) {
                    topics.push(topic);
                }
            }

            // ── Build client config ─────────────────────────────────
            let config = super::connector::WsClientConfig {
                url: self.url.clone(),
                auto_reconnect: self.auto_reconnect,
                max_reconnect_attempts: self.max_reconnect_attempts,
                keepalive_interval: if self.keepalive_ms > 0 {
                    Some(Duration::from_millis(self.keepalive_ms))
                } else {
                    None
                },
                max_offline_queue: self.max_offline_queue,
                subscribe_topics: topics,
                late_join: self.late_join,
            };

            // ── Build the connector ─────────────────────────────────
            let connector = WsClientConnectorImpl::connect(config, router, db)
                .await
                .map_err(|e| aimdb_core::DbError::RuntimeError {
                    message: format!("WS client connect failed: {}", e).into(),
                })?;

            // ── Spawn outbound publishers ────────────────────────────
            connector
                .spawn_outbound_publishers(db, outbound_routes)
                .map_err(|e| aimdb_core::DbError::RuntimeError {
                    message: format!("WS client outbound setup failed: {}", e).into(),
                })?;

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        })
    }
}
