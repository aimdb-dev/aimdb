//! Per-client WebSocket session management.
//!
//! Each accepted connection spawns three cooperating tasks:
//!
//! 1. **Send loop** — drains the per-client `mpsc` channel and writes frames to
//!    the WebSocket.
//! 2. **Recv loop** — reads frames from the WebSocket and dispatches
//!    `subscribe`, `unsubscribe`, `write`, and `ping` messages.
//! 3. A **cleanup** fence — unregisters the client from the [`ClientManager`]
//!    when either loop finishes.
//!
//! The session receives an already-authenticated [`ClientInfo`] and the shared
//! [`ClientManager`] / inbound [`Router`] from the server.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::{
    auth::{AuthHandler, ClientId, ClientInfo},
    client_manager::ClientManager,
    protocol::{ClientMessage, ErrorCode},
};

// Re-export so server.rs can use it easily.
pub use aimdb_core::router::Router;

// ════════════════════════════════════════════════════════════════════
// Session context
// ════════════════════════════════════════════════════════════════════

/// Shared context injected into every session.
#[derive(Clone)]
pub(crate) struct SessionContext {
    pub client_mgr: ClientManager,
    /// Inbound router: maps WebSocket topics → AimDB producers.
    pub router: Arc<Router>,
    pub auth: Arc<dyn AuthHandler>,
    /// Channel capacity used when registering a new client.
    pub channel_capacity: usize,
    /// Whether to send current values on subscribe (late-join).
    pub late_join: bool,
    /// Snapshot provider: topic → serialized current value.
    ///
    /// Set by the connector builder after collecting outbound routes.
    pub snapshot_provider: Arc<dyn SnapshotProvider>,
    /// Topics to subscribe every new client to automatically on connect.
    ///
    /// Use `["#"]` to push all data to all clients without requiring an
    /// explicit `{"type":"subscribe"}` message from the client.
    pub auto_subscribe_topics: Vec<String>,
}

/// Provides the current serialized value of a record for late-join snapshots.
pub(crate) trait SnapshotProvider: Send + Sync + 'static {
    /// Return the latest serialized value for the given topic, if available.
    fn snapshot(&self, topic: &str) -> Option<Vec<u8>>;
}

/// A snapshot provider that always returns `None` (used when late-join is disabled
/// or no snapshot data is available).
pub(crate) struct NoSnapshot;

impl SnapshotProvider for NoSnapshot {
    fn snapshot(&self, _topic: &str) -> Option<Vec<u8>> {
        None
    }
}

/// A snapshot provider backed by a `HashMap<topic, bytes>`.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) struct MapSnapshot(pub std::collections::HashMap<String, Vec<u8>>);

#[cfg(test)]
impl SnapshotProvider for MapSnapshot {
    fn snapshot(&self, topic: &str) -> Option<Vec<u8>> {
        self.0.get(topic).cloned()
    }
}

// ════════════════════════════════════════════════════════════════════
// Session entry point
// ════════════════════════════════════════════════════════════════════

/// Drive a single WebSocket connection to completion.
///
/// This function is `await`ed inside `tokio::spawn` by the Axum upgrade handler.
pub(crate) async fn run_session(socket: WebSocket, info: ClientInfo, ctx: SessionContext) {
    let id = info.id;

    // Register client and obtain the per-client receiver
    let (_, rx) = ctx.client_mgr.register(info, ctx.channel_capacity);

    // Auto-subscribe: subscribe all clients to the configured topics immediately
    // on connect, without requiring a Subscribe message from the client.
    if !ctx.auto_subscribe_topics.is_empty() {
        ctx.client_mgr.subscribe(id, &ctx.auto_subscribe_topics);
    }

    #[cfg(feature = "tracing")]
    tracing::debug!("{}: session started", id);

    let (ws_sender, ws_receiver) = socket.split();

    // Spawn the send loop (mpsc receiver → WebSocket sender)
    let mgr_send = ctx.client_mgr.clone();
    let send_handle = tokio::spawn(send_loop(ws_sender, rx, id));

    // Run the receive loop in-place (WebSocket receiver → router/subscriptions)
    recv_loop(ws_receiver, id, ctx).await;

    // Receiving finished; abort sender and unregister
    send_handle.abort();
    mgr_send.unregister(id);

    #[cfg(feature = "tracing")]
    tracing::debug!("{}: session ended", id);
}

// ════════════════════════════════════════════════════════════════════
// Send loop
// ════════════════════════════════════════════════════════════════════

async fn send_loop(
    mut ws_sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Message>,
    #[allow(unused_variables)] id: ClientId,
) {
    while let Some(msg) = rx.recv().await {
        if ws_sender.send(msg).await.is_err() {
            #[cfg(feature = "tracing")]
            tracing::debug!("{}: send failed — closing", id);
            break;
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Receive loop
// ════════════════════════════════════════════════════════════════════

async fn recv_loop(
    mut ws_receiver: futures_util::stream::SplitStream<WebSocket>,
    id: ClientId,
    ctx: SessionContext,
) {
    while let Some(result) = ws_receiver.next().await {
        let raw = match result {
            Ok(msg) => msg,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("{}: recv error: {}", id, _e);
                break;
            }
        };

        match raw {
            Message::Text(text) => {
                handle_text(id, text.as_str(), &ctx).await;
            }
            Message::Binary(bytes) => {
                handle_text(id, &String::from_utf8_lossy(&bytes), &ctx).await;
            }
            Message::Close(_) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("{}: received close frame", id);
                break;
            }
            // WebSocket ping/pong frames are handled transparently by axum.
            _ => {}
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Message dispatch
// ════════════════════════════════════════════════════════════════════

async fn handle_text(id: ClientId, text: &str, ctx: &SessionContext) {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("{}: invalid JSON from client: {}", id, _e);
            ctx.client_mgr
                .send_error(
                    id,
                    ErrorCode::SerializationError,
                    None,
                    "Invalid JSON message",
                )
                .await;
            return;
        }
    };

    match msg {
        ClientMessage::Subscribe { topics } => handle_subscribe(id, topics, ctx).await,
        ClientMessage::Unsubscribe { topics } => {
            ctx.client_mgr.unsubscribe(id, &topics);
        }
        ClientMessage::Write { topic, payload } => handle_write(id, topic, payload, ctx).await,
        ClientMessage::Ping => {
            ctx.client_mgr.send_pong(id).await;
        }
    }
}

async fn handle_subscribe(id: ClientId, topics: Vec<String>, ctx: &SessionContext) {
    // Authorise each requested pattern
    let client_info = match ctx.client_mgr.client_info(id) {
        Some(i) => i,
        None => return,
    };

    let mut allowed = Vec::new();

    for topic in &topics {
        if ctx.auth.authorize_subscribe(&client_info, topic).await {
            allowed.push(topic.clone());
        } else {
            ctx.client_mgr
                .send_error(
                    id,
                    ErrorCode::Forbidden,
                    Some(topic.clone()),
                    "Not authorised to subscribe to this topic",
                )
                .await;
        }
    }

    if allowed.is_empty() {
        return;
    }

    // Register subscriptions
    let confirmed = ctx.client_mgr.subscribe(id, &allowed);

    // Send acknowledgement
    ctx.client_mgr.send_subscribed(id, confirmed.clone()).await;

    // Late-join: send current values for each exact topic pattern that resolves
    if ctx.late_join {
        for pattern in confirmed {
            if let Some(bytes) = ctx.snapshot_provider.snapshot(&pattern) {
                ctx.client_mgr.send_snapshot(id, &pattern, &bytes).await;
            }
        }
    }
}

async fn handle_write(
    id: ClientId,
    topic: String,
    payload: serde_json::Value,
    ctx: &SessionContext,
) {
    // Authorise
    let client_info = match ctx.client_mgr.client_info(id) {
        Some(i) => i,
        None => return,
    };

    if !ctx.auth.authorize_write(&client_info, &topic).await {
        ctx.client_mgr
            .send_error(
                id,
                ErrorCode::Forbidden,
                Some(topic.clone()),
                "Write permission denied",
            )
            .await;
        return;
    }

    // Serialize payload back to bytes for the router
    let bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(_e) => {
            ctx.client_mgr
                .send_error(
                    id,
                    ErrorCode::SerializationError,
                    Some(topic.clone()),
                    "Failed to re-serialize payload",
                )
                .await;
            return;
        }
    };

    // Dispatch through the inbound router
    if let Err(_e) = ctx.router.route(&topic, &bytes).await {
        #[cfg(feature = "tracing")]
        tracing::warn!("{}: write routing failed for '{}': {}", id, topic, _e);

        ctx.client_mgr
            .send_error(
                id,
                ErrorCode::UnknownTopic,
                Some(topic),
                "No inbound route for this topic",
            )
            .await;
    }
}
