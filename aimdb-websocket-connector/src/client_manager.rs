//! Shared client registry and topic-based fan-out.
//!
//! [`ClientManager`] tracks all connected WebSocket clients and their topic
//! subscriptions.  When an outbound publisher task receives a new value it calls
//! [`ClientManager::broadcast`] which serializes the payload once and delivers it
//! to every client that has a matching subscription pattern.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use axum::extract::ws::Message;
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    auth::{ClientId, ClientInfo},
    protocol::{topic_matches, ErrorCode, ServerMessage},
};

// ════════════════════════════════════════════════════════════════════
// Per-client state
// ════════════════════════════════════════════════════════════════════

/// State tracked for each connected WebSocket client.
pub(crate) struct ClientState {
    pub info: ClientInfo,
    /// Channel used to push messages to the client's send loop.
    pub sender: mpsc::Sender<Message>,
    /// Topic patterns this client has subscribed to.
    pub subscriptions: Vec<String>,
}

// ════════════════════════════════════════════════════════════════════
// ClientManager
// ════════════════════════════════════════════════════════════════════

/// Shared registry of connected clients with subscription-based fan-out.
///
/// Cloning this type is cheap — all instances share the same underlying data.
#[derive(Clone)]
pub struct ClientManager {
    /// Map from ClientId → per-client state.
    ///
    /// `DashMap` is used instead of `RwLock<HashMap>` to minimise lock contention
    /// when many publisher tasks are broadcasting concurrently.
    clients: Arc<DashMap<u64, ClientState>>,
    /// Monotonically-increasing counter for generating unique `ClientId`s.
    next_id: Arc<AtomicU64>,
}

impl ClientManager {
    /// Create a new, empty client registry.
    pub fn new() -> Self {
        Self {
            clients: Arc::new(DashMap::new()),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Register a new client and return its id together with the message receiver.
    ///
    /// The caller (session task) owns the `mpsc::Receiver` and drives the
    /// WebSocket send loop.
    pub fn register(
        &self,
        info: ClientInfo,
        channel_capacity: usize,
    ) -> (ClientId, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let state = ClientState {
            info,
            sender: tx,
            subscriptions: Vec::new(),
        };
        let raw_id = state.info.id.0;
        self.clients.insert(raw_id, state);
        (ClientId(raw_id), rx)
    }

    /// Remove a client from the registry (called when the connection closes).
    pub fn unregister(&self, id: ClientId) {
        self.clients.remove(&id.0);
    }

    /// Return the number of currently connected clients.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Allocate a new unique `ClientId`.
    pub fn next_client_id(&self) -> ClientId {
        ClientId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    // ────────────────────────────────────────────────────────────────
    // Subscription management (called from session recv loop)
    // ────────────────────────────────────────────────────────────────

    /// Add subscription patterns for the given client.
    ///
    /// Returns the list of newly-added patterns (already-subscribed patterns
    /// are silently included without duplication).
    pub fn subscribe(&self, id: ClientId, patterns: &[String]) -> Vec<String> {
        let mut added = Vec::new();
        if let Some(mut entry) = self.clients.get_mut(&id.0) {
            for pat in patterns {
                if !entry.subscriptions.contains(pat) {
                    entry.subscriptions.push(pat.clone());
                }
                added.push(pat.clone());
            }
        }
        added
    }

    /// Remove subscription patterns for the given client.
    pub fn unsubscribe(&self, id: ClientId, patterns: &[String]) {
        if let Some(mut entry) = self.clients.get_mut(&id.0) {
            entry.subscriptions.retain(|s| !patterns.contains(s));
        }
    }

    /// Returns `true` if the client has at least one matching subscription for `topic`.
    pub fn is_subscribed(&self, id: ClientId, topic: &str) -> bool {
        self.clients
            .get(&id.0)
            .map(|e| e.subscriptions.iter().any(|p| topic_matches(p, topic)))
            .unwrap_or(false)
    }

    // ────────────────────────────────────────────────────────────────
    // Fan-out
    // ────────────────────────────────────────────────────────────────

    /// Broadcast a serialized `data` payload to all clients subscribed to `topic`.
    ///
    /// The payload bytes (from the record serializer) are parsed as JSON once;
    /// if parsing fails the raw bytes are embedded as a JSON string.
    pub async fn broadcast(&self, topic: &str, payload_bytes: &[u8]) {
        let payload = parse_payload(payload_bytes);
        let ts = crate::protocol::now_ms();

        let msg = ServerMessage::Data {
            topic: topic.to_string(),
            payload: Some(payload),
            ts,
        };

        let text = match serde_json::to_string(&msg) {
            Ok(t) => t,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::error!(
                    "Failed to serialize data message for topic '{}': {}",
                    topic,
                    _e
                );
                return;
            }
        };

        let ws_msg = Message::Text(text.into());

        // Iterate clients without holding a write lock
        let ids: Vec<u64> = self
            .clients
            .iter()
            .filter_map(|entry| {
                if entry.subscriptions.iter().any(|p| topic_matches(p, topic)) {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect();

        for raw_id in ids {
            if let Some(entry) = self.clients.get(&raw_id) {
                let _ = entry.sender.try_send(ws_msg.clone());
            }
        }
    }

    /// Broadcast raw payload bytes directly to all subscribed clients as a
    /// WebSocket text frame — **no `ServerMessage` envelope**.
    ///
    /// Use this (with `raw_payload = true` on the connector builder) when the
    /// serializer already produces the complete JSON the client expects.
    pub async fn broadcast_raw(&self, topic: &str, payload_bytes: &[u8]) {
        let text = match std::str::from_utf8(payload_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                #[cfg(feature = "tracing")]
                tracing::error!("broadcast_raw: payload for '{}' is not valid UTF-8", topic);
                return;
            }
        };

        let ws_msg = Message::Text(text.into());

        let ids: Vec<u64> = self
            .clients
            .iter()
            .filter_map(|entry| {
                if entry.subscriptions.iter().any(|p| topic_matches(p, topic)) {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect();

        for raw_id in ids {
            if let Some(entry) = self.clients.get(&raw_id) {
                let _ = entry.sender.try_send(ws_msg.clone());
            }
        }
    }

    /// Send a snapshot (late-join current value) to a single client.
    pub async fn send_snapshot(&self, id: ClientId, topic: &str, payload_bytes: &[u8]) {
        let payload = parse_payload(payload_bytes);
        let msg = ServerMessage::Snapshot {
            topic: topic.to_string(),
            payload: Some(payload),
        };

        self.send_to(id, &msg).await;
    }

    /// Send an error message to a single client.
    pub async fn send_error(
        &self,
        id: ClientId,
        code: ErrorCode,
        topic: Option<String>,
        message: impl Into<String>,
    ) {
        let msg = ServerMessage::Error {
            code,
            topic,
            message: message.into(),
        };
        self.send_to(id, &msg).await;
    }

    /// Send a `subscribed` acknowledgement to a single client.
    pub async fn send_subscribed(&self, id: ClientId, topics: Vec<String>) {
        let msg = ServerMessage::Subscribed { topics };
        self.send_to(id, &msg).await;
    }

    /// Send a `pong` to a single client.
    pub async fn send_pong(&self, id: ClientId) {
        self.send_to(id, &ServerMessage::Pong).await;
    }

    // ────────────────────────────────────────────────────────────────
    // Helpers
    // ────────────────────────────────────────────────────────────────

    async fn send_to(&self, id: ClientId, msg: &ServerMessage) {
        let text = match serde_json::to_string(msg) {
            Ok(t) => t,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::error!("Failed to serialize message: {}", _e);
                return;
            }
        };

        if let Some(entry) = self.clients.get(&id.0) {
            let _ = entry.sender.try_send(Message::Text(text.into()));
        }
    }

    /// Return the `ClientInfo` for the given id, if still connected.
    pub fn client_info(&self, id: ClientId) -> Option<ClientInfo> {
        self.clients.get(&id.0).map(|e| e.info.clone())
    }

    /// Returns a snapshot of (topic, subscribed-client-count) pairs for monitoring.
    pub fn subscription_stats(&self) -> HashMap<String, usize> {
        let mut stats: HashMap<String, usize> = HashMap::new();
        for entry in self.clients.iter() {
            for pat in &entry.subscriptions {
                *stats.entry(pat.clone()).or_insert(0) += 1;
            }
        }
        stats
    }
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new()
    }
}

// ════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════

/// Parse raw bytes as JSON, falling back to a JSON string if parsing fails.
fn parse_payload(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

// ════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Permissions;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn dummy_info(id: u64) -> ClientInfo {
        ClientInfo {
            id: ClientId(id),
            remote_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234),
            permissions: Permissions::allow_all(),
        }
    }

    #[tokio::test]
    async fn register_and_unregister() {
        let mgr = ClientManager::new();
        let info = dummy_info(1);
        let (id, _rx) = mgr.register(info, 16);
        assert_eq!(mgr.client_count(), 1);
        mgr.unregister(id);
        assert_eq!(mgr.client_count(), 0);
    }

    #[tokio::test]
    async fn subscribe_and_broadcast() {
        let mgr = ClientManager::new();
        let info = dummy_info(42);
        let (id, mut rx) = mgr.register(info, 16);
        mgr.subscribe(id, &["sensors/#".to_string()]);

        mgr.broadcast("sensors/temperature/vienna", b"22.5").await;

        let msg = rx.recv().await.expect("should receive message");
        if let Message::Text(text) = msg {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "data");
            assert_eq!(v["topic"], "sensors/temperature/vienna");
        } else {
            panic!("expected text message");
        }
    }

    #[tokio::test]
    async fn no_broadcast_when_not_subscribed() {
        let mgr = ClientManager::new();
        let info = dummy_info(7);
        let (id, mut rx) = mgr.register(info, 16);
        mgr.subscribe(id, &["commands/#".to_string()]);

        // Broadcast to a topic the client is NOT subscribed to
        mgr.broadcast("sensors/temperature/vienna", b"22.5").await;

        // Channel should be empty
        assert!(rx.try_recv().is_err());
    }
}
