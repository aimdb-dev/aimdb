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
//!        ├─ db.collect_outbound_routes("ws-client") → outbound futures
//!        ├─ connect to remote WebSocket server
//!        ├─ build connector_future (read + write + keepalive + reconnect)
//!        ├─ build outbound publisher futures
//!        └─ return Vec<BoxFuture> (drained by AimDbRunner)
//! ```

use std::pin::Pin;

use aimdb_core::session::{pump_client, run_client, ClientConfig};
use aimdb_core::ConnectorBuilder;

use crate::codec::WsCodec;
use crate::transport::WsDialer;

// ════════════════════════════════════════════════════════════════════
// Builder
// ════════════════════════════════════════════════════════════════════

/// Builder for the AimDB WebSocket client connector.
///
/// Connects *out* to a remote WebSocket server for direct AimDB-to-AimDB sync.
///
/// # Example
///
/// ```no_run
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
}

impl WsClientConnectorBuilder {
    /// Create a new builder targeting the given WebSocket URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auto_reconnect: true,
            max_reconnect_attempts: 0,
            keepalive_ms: 30_000,
            max_offline_queue: 256,
            subscribe_topics: Vec::new(),
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
    /// ```no_run
    /// # use aimdb_websocket_connector::WsClientConnector;
    /// WsClientConnector::new("wss://cloud/ws")
    ///     .with_subscribe_topics(["sensors/#", "config/#"]);
    /// ```
    pub fn with_subscribe_topics(
        mut self,
        topics: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.subscribe_topics = topics.into_iter().map(Into::into).collect();
        self
    }
}

// ════════════════════════════════════════════════════════════════════
// ConnectorBuilder impl
// ════════════════════════════════════════════════════════════════════

type BoxFuture = Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

impl ConnectorBuilder for WsClientConnectorBuilder {
    fn scheme(&self) -> &str {
        "ws-client"
    }

    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb,
    ) -> Pin<Box<dyn core::future::Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>>
    {
        Box::pin(async move {
            // ── Engine config from the WS-specific knobs (doc 039 § 5) ──
            // Reconnect/keepalive/offline-queue are now `ClientConfig`/engine
            // concerns; `topic_routed_subs` keys the demux by topic (the WS wire
            // pushes `Data{topic}` with no id).
            let config = ClientConfig {
                reconnect: self.auto_reconnect,
                reconnect_delay: 200,
                max_reconnect_delay: 30_000,
                max_reconnect_attempts: self.max_reconnect_attempts,
                keepalive_interval: if self.keepalive_ms > 0 {
                    Some(self.keepalive_ms)
                } else {
                    None
                },
                max_offline_queue: self.max_offline_queue,
                topic_routed_subs: true,
                sends_hello: false,
            };

            // ── Drive the shared client engine + record-mirroring pumps ──
            // Like `UdsClient`: `run_client` owns demux/reconnect/keepalive over
            // the WS `Dialer` + per-connection `WsCodec`; `pump_client` wires
            // `link_to`/`link_from` routes to the handle.
            // The runtime's clock drives reconnect backoff/keepalive.
            let (handle, engine_fut) = run_client(
                WsDialer::new(self.url.clone()),
                WsCodec::new(),
                config,
                db.runtime_ops(),
            );
            let mut futures = pump_client(db, "ws-client", &handle);
            futures.push(engine_fut);
            Ok(futures)
        })
    }
}
