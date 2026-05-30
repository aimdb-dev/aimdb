//! Shared per-topic broadcast bus (Phase 4 — doc 039 § 3).
//!
//! [`ClientManager`] is the **fan-out bridge** behind `Dispatch::subscribe`: one
//! record update reaches every matching subscription. Each `WsSession::subscribe`
//! registers a per-subscription channel and gets back a [`BoxStream`] of raw
//! record-value [`Payload`]s; the per-connection [`WsCodec`](crate::codec) wraps
//! each into a `ServerMessage::Data` on encode. The outbound record→broadcast
//! tasks ([`crate::connector`]) feed [`broadcast`](ClientManager::broadcast).
//!
//! This replaces the pre-Phase-4 model where the manager owned per-client
//! `mpsc::Sender<Message>` channels and formatted `ServerMessage`s itself — that
//! formatting now lives in the codec, and the per-connection send half is owned
//! by `run_session`.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use aimdb_core::{BoxStream, Payload};
use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::{
    auth::ClientId,
    codec::parse_payload,
    protocol::{now_ms, topic_matches, ServerMessage},
};

/// One live subscription: a wildcard pattern + the channel feeding its stream.
struct SubEntry {
    pattern: String,
    tx: mpsc::UnboundedSender<Payload>,
}

/// Shared per-topic broadcast bus. Cloning is cheap (all clones share state).
#[derive(Clone)]
pub struct ClientManager {
    /// sub-id → subscription entry.
    subs: Arc<DashMap<u64, SubEntry>>,
    /// Allocator for subscription ids.
    next_sub: Arc<AtomicU64>,
    /// Allocator for client ids (assigned at the HTTP upgrade).
    next_client: Arc<AtomicU64>,
    /// Live connection count (for the health endpoint).
    connections: Arc<AtomicU64>,
    /// Mirrors the builder's `with_raw_payload`: when set, `broadcast` ships the
    /// serializer bytes verbatim instead of wrapping them in a `Data` envelope.
    raw_payload: bool,
}

impl ClientManager {
    /// Create a new, empty bus. `raw_payload` mirrors the builder flag.
    pub fn new(raw_payload: bool) -> Self {
        Self {
            subs: Arc::new(DashMap::new()),
            next_sub: Arc::new(AtomicU64::new(1)),
            next_client: Arc::new(AtomicU64::new(1)),
            connections: Arc::new(AtomicU64::new(0)),
            raw_payload,
        }
    }

    /// Allocate a new unique [`ClientId`] (called at the HTTP upgrade).
    pub fn next_client_id(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    /// Number of live connections (informational, for `/health`).
    pub fn client_count(&self) -> usize {
        self.connections.load(Ordering::Relaxed) as usize
    }

    /// RAII guard: increments the connection count now, decrements on drop.
    pub(crate) fn connection_guard(&self) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::Relaxed);
        ConnectionGuard {
            connections: self.connections.clone(),
        }
    }

    /// Register a subscription for `pattern`; returns its id and the stream of
    /// matching record-value payloads. Dropping the stream ends the subscription
    /// (the next [`broadcast`](Self::broadcast) prunes the dead entry).
    pub fn subscribe(&self, pattern: &str) -> (u64, BoxStream<'static, Payload>) {
        let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel::<Payload>();
        self.subs.insert(
            id,
            SubEntry {
                pattern: pattern.to_string(),
                tx,
            },
        );
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        (id, Box::pin(stream))
    }

    /// Explicitly drop a subscription (on `Unsubscribe`).
    pub fn unsubscribe(&self, sub_id: u64) {
        self.subs.remove(&sub_id);
    }

    /// Fan a serialized record-value out to every subscription whose pattern
    /// matches `topic`. Dead subscriptions (dropped streams) are pruned.
    pub async fn broadcast(&self, topic: &str, payload_bytes: &[u8]) {
        // Serialize the complete wire frame **once** here — the bus is the only
        // place the real topic + value meet, and doing it once (vs once per
        // subscriber in the codec) keeps fan-out O(1). The codec writes the
        // result verbatim. The same finished bytes are `Arc`-shared to every
        // matching subscription (a refcount bump, no per-subscriber copy).
        let frame = if self.raw_payload {
            payload_bytes.to_vec()
        } else {
            match serde_json::to_vec(&ServerMessage::Data {
                topic: topic.to_string(),
                payload: Some(parse_payload(payload_bytes)),
                ts: now_ms(),
            }) {
                Ok(f) => f,
                Err(_) => return,
            }
        };
        let payload = Payload::from(frame.as_slice());
        let mut dead: Vec<u64> = Vec::new();
        for entry in self.subs.iter() {
            if topic_matches(&entry.pattern, topic) && entry.tx.send(payload.clone()).is_err() {
                dead.push(*entry.key());
            }
        }
        for id in dead {
            self.subs.remove(&id);
        }
    }

    /// Number of live subscriptions (for monitoring/tests).
    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new(false)
    }
}

/// Decrements the connection count when dropped (held by `WsSession`).
pub(crate) struct ConnectionGuard {
    connections: Arc<AtomicU64>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn broadcast_reaches_matching_subscriptions() {
        let mgr = ClientManager::new(false);
        let (_id, mut stream) = mgr.subscribe("sensors/#");

        mgr.broadcast("sensors/temp/vienna", b"22.5").await;

        // Delivery is the complete, pre-serialized `Data` frame (built once in
        // broadcast) carrying the real topic — even for the wildcard sub.
        let payload = stream.next().await.expect("should receive");
        match serde_json::from_slice::<ServerMessage>(&payload).unwrap() {
            ServerMessage::Data { topic, payload, .. } => {
                assert_eq!(topic, "sensors/temp/vienna");
                assert_eq!(payload, Some(serde_json::json!(22.5)));
            }
            _ => panic!("expected Data"),
        }
    }

    #[tokio::test]
    async fn non_matching_topic_is_not_delivered() {
        use futures_util::FutureExt;
        let mgr = ClientManager::new(false);
        let (_id, mut stream) = mgr.subscribe("commands/#");
        mgr.broadcast("sensors/temp", b"22.5").await;
        // Nothing queued: the next() future is not ready.
        assert!(stream.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn fan_out_to_n_subscribers() {
        let mgr = ClientManager::new(false);
        let mut streams: Vec<_> = (0..5).map(|_| mgr.subscribe("#").1).collect();
        mgr.broadcast("any/topic", b"\"v\"").await;
        for s in &mut streams {
            let frame = s.next().await.unwrap();
            assert!(matches!(
                serde_json::from_slice::<ServerMessage>(&frame).unwrap(),
                ServerMessage::Data { topic, .. } if topic == "any/topic"
            ));
        }
    }

    #[tokio::test]
    async fn dropped_stream_is_pruned() {
        let mgr = ClientManager::new(false);
        let (_id, stream) = mgr.subscribe("#");
        assert_eq!(mgr.subscription_count(), 1);
        drop(stream);
        mgr.broadcast("t", b"v").await;
        assert_eq!(mgr.subscription_count(), 0);
    }
}
