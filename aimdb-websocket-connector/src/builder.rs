//! Builder for the WebSocket connector.
//!
//! [`WebSocketConnectorBuilder`] implements [`ConnectorBuilder<R>`] from
//! `aimdb-core`, following the same pattern as `MqttConnectorBuilder`.
//!
//! # Lifecycle
//!
//! ```text
//! AimDbBuilder::build()
//!   └─ WebSocketConnectorBuilder::build(&db)
//!        ├─ db.collect_inbound_routes("ws")   → Router
//!        ├─ db.collect_outbound_routes("ws")   → outbound tasks
//!        ├─ start Axum / WebSocket server
//!        └─ return Arc<WebSocketConnectorImpl>
//! ```

use std::{
    any::TypeId,
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{Arc, Mutex},
};

use aimdb_data_contracts::for_each_streamable;

use aimdb_core::{router::RouterBuilder, ConnectorBuilder};
use axum::Router as AxumRouter;

use crate::{
    auth::{AuthHandler, DynAuthHandler, NoAuth},
    client_manager::ClientManager,
    connector::WebSocketConnectorImpl,
    server::start_server,
    session::{NoQuery, NoSnapshot, QueryHandler, SessionContext, SnapshotProvider},
};
use aimdb_ws_protocol::TopicInfo;

// ════════════════════════════════════════════════════════════════════
// Builder
// ════════════════════════════════════════════════════════════════════

/// Builder for the AimDB WebSocket connector.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_websocket_connector::WebSocketConnector;
///
/// let connector = WebSocketConnector::new()
///     .bind("0.0.0.0:8080")
///     .path("/ws")
///     .with_late_join(true)
///     .with_max_clients(500);
/// ```
pub struct WebSocketConnectorBuilder {
    bind_addr: SocketAddr,
    ws_path: String,
    auth: DynAuthHandler,
    late_join: bool,
    max_clients: usize,
    channel_capacity: usize,
    additional_routes: Option<AxumRouter>,
    /// Topics to subscribe every new client to automatically on connect.
    ///
    /// When non-empty, clients receive data on these topics immediately after
    /// the WebSocket handshake without having to send a `Subscribe` message.
    /// Use `["#"]` to push all topics to every client.
    auto_subscribe_topics: Vec<String>,
    /// When `true`, the serialized payload bytes are sent directly as the
    /// WebSocket text frame — no `ServerMessage::Data` envelope.
    ///
    /// Combine with a serializer that produces a complete flat JSON object
    /// (including `"type"` and `"node_id"`) to speak a custom protocol.
    raw_payload: bool,
    /// Handler for client `query` messages (history retrieval).
    query_handler: Arc<dyn QueryHandler>,
}

impl Default for WebSocketConnectorBuilder {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            ws_path: "/ws".to_string(),
            auth: Arc::new(NoAuth),
            late_join: true,
            max_clients: 1024,
            channel_capacity: 256,
            additional_routes: None,
            auto_subscribe_topics: Vec::new(),
            raw_payload: false,
            query_handler: Arc::new(NoQuery),
        }
    }
}

impl WebSocketConnectorBuilder {
    /// Create a new builder with sensible defaults.
    ///
    /// Defaults:
    /// - bind address: `0.0.0.0:8080`
    /// - WebSocket path: `/ws`
    /// - auth: allow all
    /// - late-join snapshots: enabled
    /// - max clients: 1 024
    /// - per-client channel capacity: 256
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the TCP address to bind the WebSocket server to.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// .bind("0.0.0.0:9090")
    /// .bind(([127, 0, 0, 1], 8765))
    /// ```
    pub fn bind(mut self, addr: impl ToSocketAddrs) -> Self {
        self.bind_addr = addr
            .to_socket_addrs()
            .expect("invalid bind address")
            .next()
            .expect("bind address resolved to no addresses");
        self
    }

    /// Set the URL path for the WebSocket upgrade endpoint (default: `"/ws"`).
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.ws_path = path.into();
        self
    }

    /// Use JSON encoding (currently the only supported encoding, this is the
    /// default and is provided for explicitness).
    pub fn with_json_encoding(self) -> Self {
        self
    }

    /// Plug in a custom authentication / authorization handler.
    pub fn with_auth(mut self, handler: impl AuthHandler + 'static) -> Self {
        self.auth = Arc::new(handler);
        self
    }

    /// Enable or disable late-join snapshots (default: `true`).
    ///
    /// When enabled, a client that subscribes to a topic immediately receives
    /// the current value (if one is available) as a `snapshot` message before
    /// live `data` pushes start.
    pub fn with_late_join(mut self, enabled: bool) -> Self {
        self.late_join = enabled;
        self
    }

    /// Set the maximum number of concurrent WebSocket clients (default: 1 024).
    ///
    /// Currently informational — used for pre-allocating the client map.
    pub fn with_max_clients(mut self, max: usize) -> Self {
        self.max_clients = max;
        self
    }

    /// Set the per-client send-buffer capacity in messages (default: 256).
    ///
    /// If the buffer fills up (slow client), messages are silently dropped via
    /// `try_send`.
    pub fn with_channel_capacity(mut self, cap: usize) -> Self {
        self.channel_capacity = cap;
        self
    }

    /// Mount additional Axum routes (e.g., REST endpoints) on the same server.
    ///
    /// The extra routes are merged into the connector's Axum application so that
    /// REST and WebSocket traffic can share a single port.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axum::{routing::get, Router};
    ///
    /// let rest = Router::new().route("/api/status", get(status_handler));
    /// let connector = WebSocketConnector::new().with_additional_routes(rest);
    /// ```
    pub fn with_additional_routes(mut self, router: AxumRouter) -> Self {
        self.additional_routes = Some(router);
        self
    }

    /// Subscribe every new client to these topic patterns immediately on connect.
    ///
    /// Clients will begin receiving data on matching topics right after the
    /// WebSocket handshake without needing to send a `Subscribe` message.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// WebSocketConnector::new()
    ///     .with_auto_subscribe(["#"])          // push everything
    ///     .with_auto_subscribe(["sensors/#"])  // only sensor topics
    /// ```
    pub fn with_auto_subscribe(
        mut self,
        topics: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.auto_subscribe_topics = topics.into_iter().map(Into::into).collect();
        self
    }

    /// Send serializer output directly as a WebSocket text frame, bypassing
    /// the `{"type":"data","topic":…,"payload":…}` envelope.
    ///
    /// Use this when the record serializers already produce the complete JSON
    /// expected by the client (e.g. `{"type":"temperature","node_id":…}`).
    pub fn with_raw_payload(mut self, enabled: bool) -> Self {
        self.raw_payload = enabled;
        self
    }

    /// Plug in a handler for client `query` messages (history retrieval).
    ///
    /// When set, clients can send `{"type":"query", "id":"…", "pattern":"*"}`
    /// and receive a `{"type":"query_result", …}` response with persisted records.
    ///
    /// Without this, query messages receive a `server_error` response.
    pub fn with_query_handler(mut self, handler: impl QueryHandler + 'static) -> Self {
        self.query_handler = Arc::new(handler);
        self
    }
}

// ════════════════════════════════════════════════════════════════════
// ConnectorBuilder impl
// ════════════════════════════════════════════════════════════════════

impl<R> ConnectorBuilder<R> for WebSocketConnectorBuilder
where
    R: aimdb_executor::Spawn + 'static,
{
    fn scheme(&self) -> &str {
        "ws"
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
            let inbound_routes = db.collect_inbound_routes("ws");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "WS connector: {} inbound routes collected",
                inbound_routes.len()
            );

            let router = Arc::new(RouterBuilder::from_routes(inbound_routes).build());

            // ── Outbound routes ──────────────────────────────────────
            let outbound_routes = db.collect_outbound_routes("ws");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "WS connector: {} outbound routes collected",
                outbound_routes.len()
            );

            // ── Shared snapshot cache (for late-join) ─────────────
            let snapshot_map: Arc<Mutex<HashMap<String, Vec<u8>>>> =
                Arc::new(Mutex::new(HashMap::new()));

            // ── Client manager ────────────────────────────────────
            let client_mgr = ClientManager::new();

            // ── Build snapshot provider ──────────────────────────
            let snapshot_provider: Arc<dyn SnapshotProvider> = if self.late_join {
                let snap = snapshot_map.clone();
                Arc::new(DynMapSnapshot(snap))
            } else {
                Arc::new(NoSnapshot)
            };

            // ── Known topics (for list_topics responses) ──────────
            // Build a TypeId → schema name map from all registered Streamable types.
            struct TypeIdMap(HashMap<TypeId, &'static str>);
            impl aimdb_data_contracts::StreamableVisitor for TypeIdMap {
                fn visit<T: aimdb_data_contracts::Streamable>(&mut self) {
                    self.0.insert(
                        TypeId::of::<T>(),
                        <T as aimdb_data_contracts::SchemaType>::NAME,
                    );
                }
            }
            let mut type_id_map = TypeIdMap(HashMap::new());
            for_each_streamable(&mut type_id_map);

            let topic_type_ids = db.collect_outbound_topic_type_ids("ws");
            let known_topics: Vec<TopicInfo> = topic_type_ids
                .into_iter()
                .map(|(topic, type_id)| {
                    let schema_type = type_id_map.0.get(&type_id).map(|s| s.to_string());
                    TopicInfo {
                        name: topic,
                        schema_type,
                    }
                })
                .collect();

            // ── Session context ───────────────────────────────────
            let session_ctx = SessionContext {
                client_mgr: client_mgr.clone(),
                router: router.clone(),
                auth: self.auth.clone(),
                channel_capacity: self.channel_capacity,
                late_join: self.late_join,
                snapshot_provider,
                auto_subscribe_topics: self.auto_subscribe_topics.clone(),
                query_handler: self.query_handler.clone(),
                known_topics,
            };

            // ── Build connector & spawn outbound publishers ───────────────
            let connector = WebSocketConnectorImpl::new(client_mgr, self.raw_payload);
            connector.spawn_outbound_publishers(db, outbound_routes, snapshot_map)?;

            // ── Start Axum server ─────────────────────────────────
            let additional = self.additional_routes.clone();
            start_server(
                self.bind_addr,
                self.ws_path.clone(),
                session_ctx,
                additional,
            );

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        })
    }
}

// ════════════════════════════════════════════════════════════════════
// Dynamic snapshot provider backed by the shared Mutex<HashMap>
// ════════════════════════════════════════════════════════════════════

struct DynMapSnapshot(Arc<Mutex<HashMap<String, Vec<u8>>>>);

impl SnapshotProvider for DynMapSnapshot {
    fn snapshot(&self, topic: &str) -> Option<Vec<u8>> {
        self.0.lock().ok()?.get(topic).cloned()
    }
}
